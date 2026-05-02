use serde_json::Value;
use query_compiler::ir::{QueryNode, SelectField};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::{parse_where_clause};

pub fn hydrate_payload_to_ir(
    ast: &SchemaAst,
    model_name: &str,
    payload: &Value, 
    alias_counter: &mut usize,
    depth: usize
) -> Result<QueryNode, String> {
    
    if depth > 10 {
        return Err("Security Exception: Maximum query depth exceeded.".to_string());
    }

    if let Some(base_def) = ast.bases.get(model_name) {
        return compile_polymorphic_read(ast, base_def, payload, alias_counter, depth);
    }

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
        let field_def = model_def.resolved_fields.iter().find(|f| &f.name == field_name)
            .ok_or_else(|| format!("Invalid field '{}' on '{}'.", field_name, model_name))?;

        match &field_def.field_type {
            AstFieldType::Scalar(type_name) => {
                if type_name == "Boolean" {
                    selections.push(SelectField::ScalarBoolean(field_name.clone()));
                } else {
                    selections.push(SelectField::Scalar(field_name.clone()));
                }
            },
            AstFieldType::ScalarArray(_) => selections.push(SelectField::ScalarArray(field_name.clone())),
            
            AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                // 3. Recursive Graph Traversal for nested relational queries
                let child_node = hydrate_payload_to_ir(ast, target_model, sub_payload, alias_counter, depth + 1)?;
                let is_list = matches!(field_def.field_type, AstFieldType::RelationArray(_));
                
                // Determine foreign key by inspecting the AST
                let mut resolved_fk = format!("{}_id", model_name.to_lowercase()); // fallback
                
                let relation_attr = field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
                let relation_name = if let Some(FieldAttribute::Relation { name, .. }) = relation_attr {
                    name.clone()
                } else {
                    None
                };

                let target_resolved_fields = if let Some(m) = ast.models.get(target_model) {
                    &m.resolved_fields
                } else if let Some(b) = ast.bases.get(target_model) {
                    &b.resolved_fields
                } else {
                    return Err(format!("Security Exception: Model '{}' undefined.", target_model));
                };
                for target_field in target_resolved_fields {
                    if let AstFieldType::Relation(ref_model) | AstFieldType::RelationArray(ref_model) = &target_field.field_type {
                        if ref_model == model_name {
                            if let Some(FieldAttribute::Relation { name: target_name, fields, .. }) = target_field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                                if relation_name == *target_name {
                                    if !fields.is_empty() {
                                        resolved_fk = fields[0].clone();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                
                let mut is_forward = false;
                
                // If we are on the child side (we own the foreign key), our own @relation holds the fields
                if let Some(FieldAttribute::Relation { fields, references, .. }) = relation_attr {
                    if !fields.is_empty() {
                        let is_pk = model_def.resolved_fields.iter().any(|f| &f.name == &fields[0] && f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)));
                        if is_pk {
                            is_forward = false;
                            resolved_fk = if !references.is_empty() { references[0].clone() } else { format!("{}_id", model_name.to_lowercase()) };
                        } else {
                            resolved_fk = fields[0].clone();
                            is_forward = true;
                        }
                    }
                }
                
                // We also need to inject the foreign key (teamId) into the INNER branches of the target polymorphic union!
                // Since child_node is a PolymorphicUnion, we must mutate its branches to select resolved_fk.
                let mut child_node = child_node;
                if !is_forward {
                    if !child_node.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                        child_node.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                    }
                    
                    if let query_compiler::ir::QueryIrSource::Polymorphic { branches, .. } = &mut child_node.source {
                        for branch in branches {
                            if !branch.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                                branch.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                            }
                        }
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
            AstFieldType::PolymorphicUnion(target_name) | AstFieldType::PolymorphicUnionArray(target_name) | AstFieldType::PolymorphicBase(target_name) | AstFieldType::PolymorphicBaseArray(target_name) => {
                let is_list = matches!(field_def.field_type, AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_));
                
                let targets = if let Some(union_targets) = ast.unions.get(target_name) {
                    union_targets.clone()
                } else if ast.bases.contains_key(target_name) {
                    let mut impls = Vec::new();
                    for model in ast.models.values() {
                        if model.resolved_bases.contains(target_name) {
                            impls.push(model.name.clone());
                        }
                    }
                    impls
                } else {
                    return Err(format!("Security Exception: Target '{}' undefined.", target_name));
                };
                    
                let mut target_fragments = std::collections::HashMap::new();
                
                // Expecting sub_payload to have keys matching the target models
                if let Some(union_queries) = sub_payload.as_object() {
                    for (target_model_name, target_payload) in union_queries {
                        if !targets.contains(target_model_name) {
                            return Err(format!("Invalid union target '{}' for union '{}'.", target_model_name, target_name));
                        }
                        
                        let fragment_node = hydrate_payload_to_ir(ast, target_model_name, target_payload, alias_counter, depth + 1)?;
                        target_fragments.insert(target_model_name.clone(), fragment_node);
                    }
                }
                
                if target_fragments.is_empty() {
                    return Err(format!("Missing union target fragments for field '{}'.", field_name));
                }

                selections.push(SelectField::Polymorphic {
                    field_name: field_name.clone(),
                    is_list,
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

    let primary_key = model_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
        .map(|f| f.name.clone())
        .unwrap_or_else(|| "__id".to_string());

    Ok(QueryNode {
        source: query_compiler::ir::QueryIrSource::Table(model_name.to_string()),
        primary_key,
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
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            unions: std::collections::HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            resolved_fields: vec![
                FieldNode { name: "__id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "tags".to_string(), field_type: AstFieldType::ScalarArray("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "password".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::UpdatedAt] },
                FieldNode { name: "posts".to_string(), field_type: AstFieldType::RelationArray("Post".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "profile".to_string(), field_type: AstFieldType::Relation("Profile".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "content".to_string(), field_type: AstFieldType::PolymorphicUnion("SearchContent".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "contents".to_string(), field_type: AstFieldType::PolymorphicUnionArray("SearchContent".to_string()), is_optional: false, attributes: vec![] },
            ]
        });

        ast.models.insert("Post".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Post".to_string(),
            resolved_fields: vec![
                FieldNode { name: "__id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "title".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "comments".to_string(), field_type: AstFieldType::RelationArray("Comment".to_string()), is_optional: false, attributes: vec![] },
            ]
        });

        ast.models.insert("Comment".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Comment".to_string(),
            resolved_fields: vec![
                FieldNode { name: "__id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "body".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "author".to_string(), field_type: AstFieldType::Relation("User".to_string()), is_optional: false, attributes: vec![] },
            ]
        });

        ast.models.insert("Profile".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Profile".to_string(),
            resolved_fields: vec![
                FieldNode { name: "bio".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
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
                "__id": true,
                "posts": {
                    "select": {
                        "__id": true,
                        "comments": {
                            "select": {
                                "__id": true,
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
        assert_eq!(ir.source, query_compiler::ir::QueryIrSource::Table("User".to_string()));
        assert_eq!(ir.alias, "t0");
        
        // posts relation
        let posts_relation = ir.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "posts")).unwrap();
        if let SelectField::Relation { query: posts_query, .. } = posts_relation {
            assert_eq!(posts_query.source, query_compiler::ir::QueryIrSource::Table("Post".to_string()));
            assert_eq!(posts_query.alias, "t1");
            
            // comments relation
            let comments_relation = posts_query.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "comments")).unwrap();
            if let SelectField::Relation { query: comments_query, .. } = comments_relation {
                assert_eq!(comments_query.source, query_compiler::ir::QueryIrSource::Table("Comment".to_string()));
                assert_eq!(comments_query.alias, "t2");
                
                // author relation
                let author_relation = comments_query.selections.iter().find(|s| matches!(s, SelectField::Relation { field_name, .. } if field_name == "author")).unwrap();
                if let SelectField::Relation { query: author_query, .. } = author_relation {
                    assert_eq!(author_query.source, query_compiler::ir::QueryIrSource::Table("User".to_string()));
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
                "__id": true,
                "posts": {
                    "select": {
                        "title": true
                    }
                }
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
        assert_eq!(ir.source, query_compiler::ir::QueryIrSource::Table("User".to_string()));
        assert_eq!(ir.alias, "t0");
        assert_eq!(ir.selections.len(), 2);
        
        // Assert relation mapping
        let relation = ir.selections.iter().find(|s| matches!(s, SelectField::Relation { .. })).unwrap();
        if let SelectField::Relation { field_name, is_list, query, .. } = relation {
            assert_eq!(field_name, "posts");
            assert!(*is_list);
            assert_eq!(query.source, query_compiler::ir::QueryIrSource::Table("Post".to_string()));
            assert_eq!(query.alias, "t1");
            assert_eq!(query.selections.len(), 2);
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
        let union_field = ir.selections.iter().find(|s| matches!(s, SelectField::Polymorphic { .. })).unwrap();
        if let SelectField::Polymorphic { field_name, is_list, target_fragments } = union_field {
            assert_eq!(field_name, "content");
            assert_eq!(*is_list, false);
            assert_eq!(target_fragments.len(), 2);
            assert!(target_fragments.contains_key("Post"));
            assert!(target_fragments.contains_key("User"));
        } else {
            panic!("Expected PolymorphicUnion");
        }
    }

    #[test]
    fn test_hydrate_polymorphic_union_array() {
        let ast = mock_ast();
        let payload = json!({
            "select": {
                "contents": {
                    "Post": { "select": { "title": true } },
                    "User": { "select": { "name": true } }
                }
            }
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
        let union_field = ir.selections.iter().find(|s| matches!(s, SelectField::Polymorphic { .. })).unwrap();
        if let SelectField::Polymorphic { field_name, is_list, target_fragments } = union_field {
            assert_eq!(field_name, "contents");
            assert_eq!(*is_list, true);
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
                    "UnknownModel": { "select": { "__id": true } }
                }
            }
        });
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap_err();
        assert!(err.contains("Invalid union target 'UnknownModel'"));
    }

    #[test]
    fn test_hydrate_limit_parsed() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "__id": true },
            "limit": 10
        });
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        assert!(matches!(ir.selections[0], SelectField::ScalarArray(_)));
    }

    #[test]
    fn test_hydrate_missing_select_block() {
        let ast = mock_ast();
        let payload = json!({ "not_select": {} });
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap_err();
        assert!(err.contains("Missing 'select' projection block"));
    }

    #[test]
    fn test_hydrate_invalid_model() {
        let ast = mock_ast();
        let payload = json!({ "select": { "__id": true } });
        let mut alias_counter = 0;
        
        let err = hydrate_payload_to_ir(&ast, "UnknownModel", &payload, &mut alias_counter, 0).unwrap_err();
        assert!(err.contains("Security Exception"));
    }

    #[test]
    fn test_hydrate_invalid_field() {
        let ast = mock_ast();
        let payload = json!({ "select": { "hacker_field": true } });
        let mut alias_counter = 0;
        
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap_err();
        assert!(err.contains("Invalid field 'hacker_field'"));
    }

    

    #[test]
    fn test_hydrate_pagination_and_filtering() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "__id": true },
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
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
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
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
            "select": { "__id": true },
            "where": { "hacker_field": "test" }
        });
        
        let mut alias_counter = 0;
        let err = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap_err();
        assert!(err.contains("Invalid field 'hacker_field' in where clause"));
    }

    

    #[test]
    fn test_hydrate_null_filters() {
        let ast = mock_ast();
        let payload = json!({
            "select": { "__id": true },
            "where": { 
                "AND": [
                    { "name": null },
                    { "tags": { "eq": null } },
                    { "profile": { "notEq": null } }
                ]
            }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0).unwrap();
        
        if let Some(WhereClause::And(clauses)) = ir.filters {
            assert_eq!(clauses.len(), 3);
            assert_eq!(clauses[0], WhereClause::Field("name".to_string(), WhereCondition::IsNull));
            assert_eq!(clauses[1], WhereClause::Field("tags".to_string(), WhereCondition::IsNull));
            assert_eq!(clauses[2], WhereClause::Field("profile".to_string(), WhereCondition::IsNotNull));
        } else {
            panic!("Expected AND where clause");
        }
    }

    #[test]
    fn test_hydrate_max_depth_exceeded() {
        let mut ast = mock_ast();
        
        // Insert a self-referencing relationship
        ast.models.get_mut("User").unwrap().resolved_fields.push(
            FieldNode { name: "manager".to_string(), field_type: AstFieldType::Relation("User".to_string()), is_optional: true, attributes: vec![] }
        );
        
        let mut select_block = json!({"__id": true});
        for _ in 0..15 {
            select_block = json!({
                "manager": {
                    "select": select_block
                }
            });
        }
        
        let payload = json!({
            "select": select_block
        });
        
        let mut alias_counter = 0;
        let result = hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter, 0);
        
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Security Exception: Maximum query depth exceeded.");
    }

    #[test]
    fn test_hydrate_custom_id_field() {
        let mut ast = mock_ast();
        
        // Let's create a new model with a custom ID field named "uuid"
        ast.models.insert("Device".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Device".to_string(),
            resolved_fields: vec![
                FieldNode { name: "uuid".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
            ]
        });
        
        let payload = json!({
            "select": { "uuid": true, "name": true }
        });
        
        let mut alias_counter = 0;
        let ir = hydrate_payload_to_ir(&ast, "Device", &payload, &mut alias_counter, 0).unwrap();
        
        assert_eq!(ir.source, query_compiler::ir::QueryIrSource::Table("Device".to_string()));
        assert_eq!(ir.primary_key, "uuid", "The IR should dynamically extract the correct primary key field name based on the @id attribute");
    }
}

fn compile_polymorphic_read(
    ast: &schema_parser::ast::SchemaAst,
    base_def: &schema_parser::ast::BaseNode,
    payload: &serde_json::Value,
    alias_counter: &mut usize,
    depth: usize
) -> Result<query_compiler::ir::QueryNode, String> {
    let mut branches = Vec::new();
    
    let mut symmetric_projection: std::collections::BTreeSet<String> = base_def.resolved_fields.iter().map(|f| f.name.clone()).collect();
    if let Some(requested_fields) = payload.get("select").and_then(|v| v.as_object()) {
        for key in requested_fields.keys() {
            symmetric_projection.insert(key.clone());
        }
    }
    
    let implementing_models: Vec<&schema_parser::ast::ModelNode> = ast.models.values()
        .filter(|m| m.resolved_bases.contains(&base_def.name))
        .collect();

    if implementing_models.is_empty() {
        return Err(format!("NoImplementations: Base '{}' has no concrete implementations.", base_def.name));
    }
    
    for model in implementing_models {
        let current_alias = format!("t{}", alias_counter);
        *alias_counter += 1;
        
        let mut inner_selections = Vec::new();
        for field_name in &symmetric_projection {
            if let Some(field_def) = model.resolved_fields.iter().find(|f| &f.name == field_name) {
                match &field_def.field_type {
                    schema_parser::ast::AstFieldType::Scalar(type_name) => {
                        if type_name == "Boolean" {
                            inner_selections.push(query_compiler::ir::SelectField::ScalarBoolean(field_name.clone()));
                        } else {
                            inner_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                        }
                    },
                    schema_parser::ast::AstFieldType::ScalarArray(_) => inner_selections.push(query_compiler::ir::SelectField::ScalarArray(field_name.clone())),
                    _ => {}
                }
            } else if field_name.starts_with("__") && field_name != "__id" && field_name != "__kind" {
                if model.name == field_name.replace("__", "") {
                    inner_selections.push(query_compiler::ir::SelectField::SyntheticNull(format!("1 AS {}", field_name.clone())));
                } else if model.resolved_bases.contains(&field_name.replace("__", "")) {
                    inner_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                } else {
                    inner_selections.push(query_compiler::ir::SelectField::SyntheticNull(format!("0 AS {}", field_name.clone())));
                }
            } else {
                return Err(format!("Invalid field '{}' on '{}'.", field_name, model.name));
            }
        }
        
        let inner_filters = if let Some(where_obj) = payload.get("where").and_then(|v| v.as_object()) {
            if where_obj.is_empty() {
                None
            } else {
                Some(crate::where_parser::parse_where_clause(ast, where_obj, model)?)
            }
        } else {
            None
        };
        
        let primary_key = model.resolved_fields.iter()
            .find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)))
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "__id".to_string());
            
        branches.push(query_compiler::ir::QueryNode {
            source: query_compiler::ir::QueryIrSource::Table(model.name.clone()),
            primary_key,
            alias: current_alias,
            selections: inner_selections,
            filters: inner_filters,
            limit: None,
            offset: None,
        });
    }
    
    let current_alias = format!("t{}", alias_counter);
    *alias_counter += 1;
    
    let mut outer_selections = Vec::new();
    let requested_fields = payload.get("select").and_then(|v| v.as_object())
        .ok_or("Missing 'select' projection block")?;
        
    for (field_name, sub_payload) in requested_fields {
        let field_def = base_def.resolved_fields.iter().find(|f| &f.name == field_name);
        
        if let Some(fd) = field_def {
            match &fd.field_type {
                schema_parser::ast::AstFieldType::Scalar(type_name) => {
                    if type_name == "Boolean" {
                        outer_selections.push(query_compiler::ir::SelectField::ScalarBoolean(field_name.clone()));
                    } else {
                        outer_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                    }
                },
                schema_parser::ast::AstFieldType::ScalarArray(_) => outer_selections.push(query_compiler::ir::SelectField::ScalarArray(field_name.clone())),
                schema_parser::ast::AstFieldType::Relation(target_model) | schema_parser::ast::AstFieldType::RelationArray(target_model) => {
                    let child_node = hydrate_payload_to_ir(ast, target_model, sub_payload, alias_counter, depth + 1)?;
                    let is_list = matches!(fd.field_type, schema_parser::ast::AstFieldType::RelationArray(_));
                    
                    let mut resolved_fk = format!("{}_id", base_def.name.to_lowercase()); 
                    let relation_attr = fd.attributes.iter().find(|a| matches!(a, schema_parser::ast::FieldAttribute::Relation { .. }));
                    let mut is_forward = true;
                    if let Some(schema_parser::ast::FieldAttribute::Relation { fields, references, .. }) = relation_attr {
                        if !fields.is_empty() {
                            let is_pk = base_def.resolved_fields.iter().any(|f| &f.name == &fields[0] && f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)));
                            if is_pk {
                                is_forward = false;
                                resolved_fk = if !references.is_empty() { references[0].clone() } else { format!("{}_id", base_def.name.to_lowercase()) };
                            } else {
                                resolved_fk = fields[0].clone();
                            }
                        } else {
                            is_forward = false;
                        }
                    }
                    if let Some(schema_parser::ast::FieldAttribute::Relation { references, .. }) = relation_attr {
                        if !is_forward && !references.is_empty() {
                            resolved_fk = references[0].clone();
                        }
                    }
                    
                    if !is_forward {
                        let target_resolved_fields = if let Some(m) = ast.models.get(target_model) {
                            &m.resolved_fields
                        } else if let Some(b) = ast.bases.get(target_model) {
                            &b.resolved_fields
                        } else {
                            return Err(format!("Security Exception: Model '{}' undefined.", target_model));
                        };
                        let relation_name = if let Some(schema_parser::ast::FieldAttribute::Relation { name: Some(n), .. }) = relation_attr { Some(n.clone()) } else { None };
                        
                        for target_field in target_resolved_fields {
                            if let schema_parser::ast::AstFieldType::Relation(ref_model) | schema_parser::ast::AstFieldType::RelationArray(ref_model) = &target_field.field_type {
                                if ref_model == &base_def.name {
                                    if let Some(schema_parser::ast::FieldAttribute::Relation { name: target_name, fields, .. }) = target_field.attributes.iter().find(|a| matches!(a, schema_parser::ast::FieldAttribute::Relation { .. })) {
                                        let name_matches = match (&relation_name, target_name) {
                                            (Some(a), Some(b)) => a == b,
                                            (None, None) => true,
                                            _ => false,
                                        };
                                        if name_matches {
                                            if !fields.is_empty() {
                                                resolved_fk = fields[0].clone();
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    
                    if is_forward {
                        symmetric_projection.insert(resolved_fk.clone()); // MUST inject FK into inner payload so outer JOIN works
                    } else {
                        let pk = base_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id))).map(|f| f.name.clone()).unwrap_or_else(|| "__id".to_string());
                        symmetric_projection.insert(pk);
                    }
                    
                    let mut child_node = child_node;
                    if !is_forward {
                        if !child_node.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                            child_node.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                        }
                        
                        if let query_compiler::ir::QueryIrSource::Polymorphic { branches, .. } = &mut child_node.source {
                            for branch in branches {
                                if !branch.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                                    branch.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                                }
                            }
                        }
                    }

                    outer_selections.push(query_compiler::ir::SelectField::Relation {
                        field_name: field_name.clone(),
                        foreign_key: resolved_fk, 
                        is_list,
                        is_forward,
                        query: Box::new(child_node),
                    });
                },
                _ => {}
            }
        } else if field_name.starts_with("__") && field_name != "__id" && field_name != "__kind" {
            outer_selections.push(query_compiler::ir::SelectField::SyntheticNull(field_name.clone()));
        } else if field_name == "__id" || field_name == "__kind" {
            // These are implicit base fields that are pushed down to concrete models
            outer_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
        } else {
            return Err(format!("Invalid field '{}' on '{}'.", field_name, base_def.name));
        }
    }
    
    let primary_key = base_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)))
        .map(|f| f.name.clone())
        .unwrap_or_else(|| "__id".to_string());

    Ok(query_compiler::ir::QueryNode {
        source: query_compiler::ir::QueryIrSource::Polymorphic {
            alias: base_def.name.clone(),
            branches,
        },
        primary_key,
        alias: current_alias,
        selections: outer_selections,
        filters: None,
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
        offset: payload.get("skip").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}
