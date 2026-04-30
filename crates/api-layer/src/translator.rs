use serde_json::Value;
use query_compiler::ir::{QueryNode, SelectField};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::{parse_where_clause};

pub fn hydrate_payload_to_ir(
    ast: &SchemaAst,
    model_name: &str,
    payload: &Value, 
    alias_counter: &mut usize
) -> Result<QueryNode, String> {
    
    // 1. Strict Schema Validation: Verify model exists physically
    let model_def = ast.models.get(model_name)
        .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

    let mut selections = Vec::new();
    let current_alias = format!("t{}", alias_counter);
    *alias_counter += 1;

    let requested_fields = payload.get("select").and_then(|v| v.as_object())
        .ok_or("Missing 'select' projection block")?;

    // 2. Dynamic Field Resolution
    for (field_name, sub_payload) in requested_fields {
        // Find field in AST
        let field_def = model_def.fields.iter().find(|f| &f.name == field_name)
            .ok_or_else(|| format!("Invalid field '{}' on '{}'.", field_name, model_name))?;

        // SECURITY INTERCEPTOR
        if field_def.attributes.iter().any(|a| matches!(a, FieldAttribute::Ignore)) {
            return Err(format!("Security Exception: Prohibited access to ignored field '{}'", field_name));
        }

        match &field_def.field_type {
            AstFieldType::Scalar(_) => selections.push(SelectField::Scalar(field_name.clone())),
            AstFieldType::ScalarArray(_) => selections.push(SelectField::ScalarArray(field_name.clone())),
            
            AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                // 3. Recursive Graph Traversal for nested relational queries
                let child_node = hydrate_payload_to_ir(ast, target_model, sub_payload, alias_counter)?;
                let is_list = matches!(field_def.field_type, AstFieldType::RelationArray(_));
                
                // Determine foreign key by inspecting the AST
                let mut resolved_fk = format!("{}_id", model_name.to_lowercase()); // fallback
                
                let relation_attr = field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
                let relation_name = if let Some(FieldAttribute::Relation { name, .. }) = relation_attr {
                    name.clone()
                } else {
                    None
                };

                let target_model_def = ast.models.get(target_model).unwrap();
                
                for target_field in &target_model_def.fields {
                    if let AstFieldType::Relation(ref_model) | AstFieldType::RelationArray(ref_model) = &target_field.field_type {
                        if ref_model == model_name {
                            if let Some(FieldAttribute::Relation { name: target_name, fields, .. }) = target_field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                                if relation_name == *target_name {
                                    if !fields.is_empty() {
                                        resolved_fk = fields[0].clone();
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                
                let mut is_forward = false;
                
                // If we are on the child side (we own the foreign key), our own @relation holds the fields
                if let Some(FieldAttribute::Relation { fields, .. }) = relation_attr {
                    if !fields.is_empty() {
                        resolved_fk = fields[0].clone();
                        is_forward = true;
                    }
                }
                
                selections.push(SelectField::Relation {
                    field_name: field_name.clone(),
                    foreign_key: resolved_fk, 
                    is_list,
                    is_forward,
                    query: Box::new(child_node),
                });
            },
            AstFieldType::PolymorphicUnion(union_name) => {
                let targets = ast.unions.get(union_name)
                    .ok_or_else(|| format!("Security Exception: Union '{}' undefined.", union_name))?;
                    
                let mut target_fragments = std::collections::HashMap::new();
                
                // Expecting sub_payload to have keys matching the target models
                if let Some(union_queries) = sub_payload.as_object() {
                    for (target_model_name, target_payload) in union_queries {
                        if !targets.contains(target_model_name) {
                            return Err(format!("Invalid union target '{}' for union '{}'.", target_model_name, union_name));
                        }
                        
                        let fragment_node = hydrate_payload_to_ir(ast, target_model_name, target_payload, alias_counter)?;
                        target_fragments.insert(target_model_name.clone(), fragment_node);
                    }
                }
                
                if target_fragments.is_empty() {
                    return Err(format!("Missing union target fragments for field '{}'.", field_name));
                }

                selections.push(SelectField::PolymorphicUnion {
                    field_name: field_name.clone(),
                    target_fragments,
                });
            }
        }
    }

    let filters = if let Some(where_obj) = payload.get("where").and_then(|v| v.as_object()) {
        if where_obj.is_empty() {
            None
        } else {
            Some(parse_where_clause(ast, where_obj, model_def)?)
        }
    } else {
        None
    };

    Ok(QueryNode {
        target_model: model_name.to_string(),
        alias: current_alias,
        selections,
        filters,
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
        offset: payload.get("skip").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_parser::ast::{ModelNode, FieldNode};
    use serde_json::json;
    use query_compiler::ir::{WhereClause, WhereCondition};

    fn mock_ast() -> SchemaAst {
        let mut ast = SchemaAst {
            models: std::collections::HashMap::new(),
            unions: std::collections::HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "tags".to_string(), field_type: AstFieldType::ScalarArray("String".to_string()), attributes: vec![] },
                FieldNode { name: "password".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![FieldAttribute::Ignore] },
                FieldNode { name: "posts".to_string(), field_type: AstFieldType::RelationArray("Post".to_string()), attributes: vec![] },
                FieldNode { name: "profile".to_string(), field_type: AstFieldType::Relation("Profile".to_string()), attributes: vec![] },
                FieldNode { name: "content".to_string(), field_type: AstFieldType::PolymorphicUnion("SearchContent".to_string()), attributes: vec![] },
            ]
        });

        ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "title".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "comments".to_string(), field_type: AstFieldType::RelationArray("Comment".to_string()), attributes: vec![] },
            ]
        });

        ast.models.insert("Comment".to_string(), ModelNode {
            name: "Comment".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "body".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "author".to_string(), field_type: AstFieldType::Relation("User".to_string()), attributes: vec![] },
            ]
        });

        ast.models.insert("Profile".to_string(), ModelNode {
            name: "Profile".to_string(),
            fields: vec![
                FieldNode { name: "bio".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
            ]
        });

        ast.unions.insert("SearchContent".to_string(), vec!["Post".to_string(), "User".to_string()]);

        ast
    }

    #[test]
    fn test_hydrate_deep_recursion() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "id": true,
                "posts": {
                    "select": {
                        "id": true,
                        "comments": {
                            "select": {
                                "id": true,
                                "author": {
                                    "select": {
                                        "name": true
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(ir.target_model, "User");
        assert_eq!(ir.alias, "t0");
        
        // posts relation
        let posts_relation = ir.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "posts")).unwrap();
        if let SelectField::Relation { query: posts_query, .. } = posts_relation {
            assert_eq!(posts_query.target_model, "Post");
            assert_eq!(posts_query.alias, "t1");
            
            // comments relation
            let comments_relation = posts_query.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "comments")).unwrap();
            if let SelectField::Relation { query: comments_query, .. } = comments_relation {
                assert_eq!(comments_query.target_model, "Comment");
                assert_eq!(comments_query.alias, "t2");
                
                // author relation
                let author_relation = comments_query.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "author")).unwrap();
                if let SelectField::Relation { query: author_query, .. } = author_relation {
                    assert_eq!(author_query.target_model, "User");
                    assert_eq!(author_query.alias, "t3");
                    assert!(author_query.selections.contains(&SelectField::Scalar("name".to_string())));
                } else {
                    panic!("Expected author Relation");
                }
            } else {
                panic!("Expected comments Relation");
            }
        } else {
            panic!("Expected posts Relation");
        }
    }

    #[test]
    fn test_hydrate_valid_payload() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "id": true,
                "posts": {
                    "select": {
                        "title": true
                    }
                }
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(ir.target_model, "User");
        assert_eq!(ir.alias, "t0");
        assert_eq!(ir.selections.len(), 2);
        
        // Assert relation mapping
        let relation = ir.selections.iter().find(|s| matches!(s, SelectField::Relation { .. })).unwrap();
        if let SelectField::Relation { field_name, is_list, query, .. } = relation {
            assert_eq!(field_name, "posts");
            assert!(*is_list);
            assert_eq!(query.target_model, "Post");
            assert_eq!(query.alias, "t1");
            assert_eq!(query.selections.len(), 1);
        } else {
            panic!("Expected Relation");
        }
    }

    #[test]
    fn test_hydrate_polymorphic_union() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "content": {
                    "Post": { "select": { "title": true } },
                    "User": { "select": { "name": true } }
                }
            }
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        let union_field = ir.selections.iter().find(|s| matches!(s, SelectField::PolymorphicUnion { .. })).unwrap();
        if let SelectField::PolymorphicUnion { field_name, target_fragments } = union_field {
            assert_eq!(field_name, "content");
            assert_eq!(target_fragments.len(), 2);
            assert!(target_fragments.contains_key("Post"));
            assert!(target_fragments.contains_key("User"));
        } else {
            panic!("Expected PolymorphicUnion");
        }
    }

    #[test]
    fn test_hydrate_polymorphic_union_invalid_target() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "content": {
                    "UnknownModel": { "select": { "id": true } }
                }
            }
        });
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Invalid union target 'UnknownModel'"));
    }

    #[test]
    fn test_hydrate_limit_parsed() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "id": true },
            "limit": 10
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        assert_eq!(ir.limit, Some(10));
    }

    #[test]
    fn test_hydrate_single_relation() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "profile": { "select": { "bio": true } }
            }
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        let relation = ir.selections.first().unwrap();
        if let SelectField::Relation { is_list, .. } = relation {
            assert!(!*is_list);
        } else {
            panic!("Expected Relation");
        }
    }

    #[test]
    fn test_hydrate_scalar_array() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "tags": true }
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        assert!(matches!(ir.selections[0], SelectField::ScalarArray(_)));
    }

    #[test]
    fn test_hydrate_missing_select_block() {
        let ast = mock_ast();
        let payload = json!({ "not_select": {} });
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Missing 'select' projection block"));
    }

    #[test]
    fn test_hydrate_invalid_model() {
        let ast = mock_ast();
        let payload = json!({ "select": { "id": true } });
        let mut alias_counter = 0;
        
        let err = hydrate_payload_to_ir(&ast, "UnknownModel", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Security Exception"));
    }

    #[test]
    fn test_hydrate_invalid_field() {
        let ast = mock_ast();
        let payload = json!({ "select": { "hacker_field": true } });
        let mut alias_counter = 0;
        
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Invalid field 'hacker_field'"));
    }

    #[test]
    fn test_hydrate_ignored_field() {
        let ast = mock_ast();
        let payload = json!({ "select": { "password": true } });
        let mut alias_counter = 0;
        
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Security Exception: Prohibited access to ignored field 'password'"));
    }

    #[test]
    fn test_hydrate_pagination_and_filtering() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "id": true },
            "limit": 10,
            "skip": 20,
            "where": {
                "AND": [
                    { "name": { "eq": "Alice" } },
                    { "OR": [
                        { "tags": { "in": ["rust", "db"] } }
                    ]}
                ]
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(ir.limit, Some(10));
        assert_eq!(ir.offset, Some(20));
        
        if let Some(WhereClause::And(clauses)) = ir.filters {
            assert_eq!(clauses.len(), 2);
            assert_eq!(clauses[0], WhereClause::Field("name".to_string(), WhereCondition::Eq("Alice".to_string())));
            
            if let WhereClause::Or(or_clauses) = &clauses[1] {
                assert_eq!(or_clauses.len(), 1);
                assert_eq!(or_clauses[0], WhereClause::Field("tags".to_string(), WhereCondition::In(vec!["rust".to_string(), "db".to_string()])));
            } else {
                panic!("Expected OR clause");
            }
        } else {
            panic!("Expected AND where clause");
        }
    }

    #[test]
    fn test_hydrate_nested_pagination_and_filtering() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "posts": {
                    "select": { "title": true },
                    "limit": 5,
                    "skip": 10,
                    "where": { "title": "First" }
                }
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        let relation = ir.selections.first().unwrap();
        if let SelectField::Relation { query, .. } = relation {
            assert_eq!(query.limit, Some(5));
            assert_eq!(query.offset, Some(10));
            assert_eq!(query.filters, Some(WhereClause::Field("title".to_string(), WhereCondition::Eq("First".to_string()))));
        } else {
            panic!("Expected Relation");
        }
    }

    #[test]
    fn test_hydrate_where_clause_invalid_field() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "id": true },
            "where": { "hacker_field": "test" }
        });
        
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Invalid field 'hacker_field' in where clause"));
    }

    #[test]
    fn test_hydrate_where_clause_ignored_field() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "id": true },
            "where": { "password": "123" }
        });
        
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap_err();
        assert!(err.contains("Security Exception: Prohibited filter on ignored field 'password'"));
    }

    #[test]
    fn test_hydrate_null_filters() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "id": true },
            "where": { 
                "AND": [
                    { "name": null },
                    { "tags": { "eq": null } },
                    { "profile": { "notEq": null } }
                ]
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
        
        if let Some(WhereClause::And(clauses)) = ir.filters {
            assert_eq!(clauses.len(), 3);
            assert_eq!(clauses[0], WhereClause::Field("name".to_string(), WhereCondition::IsNull));
            assert_eq!(clauses[1], WhereClause::Field("tags".to_string(), WhereCondition::IsNull));
            assert_eq!(clauses[2], WhereClause::Field("profile".to_string(), WhereCondition::IsNotNull));
        } else {
            panic!("Expected AND where clause");
        }
    }
}
