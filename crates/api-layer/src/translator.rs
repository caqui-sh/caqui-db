use serde_json::Value;
use query_compiler::ir::{QueryNode, SelectField};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};

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
                
                selections.push(SelectField::Relation {
                    field_name: field_name.clone(),
                    // In a production engine, foreign_key resolution is mapped from AST relation attributes
                    // but for this phase we fall back to a simple convention based on the target model.
                    foreign_key: format!("{}_id", target_model.to_lowercase()), 
                    is_list,
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

    Ok(QueryNode {
        target_model: model_name.to_string(),
        alias: current_alias,
        selections,
        filters: None, // Where-clause parsing & AST validation mapped similarly here
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_parser::ast::{ModelNode, FieldNode};
    use serde_json::json;

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
}
