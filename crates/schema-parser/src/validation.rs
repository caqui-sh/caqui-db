use std::collections::HashSet;
use crate::ast::*;

#[derive(Debug, Clone)]
pub struct ValidationError(pub String);

pub fn validate_schema(mut ast: SchemaAst) -> Result<SchemaAst, ValidationError> {
    // Pass 1: Symbol Resolution
    let mut model_symbols = HashSet::new();
    let mut union_symbols = HashSet::new();

    for model_name in ast.models.keys() {
        model_symbols.insert(model_name.clone());
    }

    for union_name in ast.unions.keys() {
        union_symbols.insert(union_name.clone());
    }

    // Pass 2: Graph Integrity & Primary Key Check
    
    // Check union targets exist
    for (union_name, targets) in &ast.unions {
        for target in targets {
            if !model_symbols.contains(target) {
                return Err(ValidationError(format!(
                    "Union '{}' references target '{}' which does not exist.",
                    union_name, target
                )));
            }
        }
    }

    // Check models
    for model in ast.models.values_mut() {
        let mut id_count = 0;
        
        for field in &mut model.fields {
            // Check for @id attribute
            let has_id = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id));
            if has_id {
                id_count += 1;
            }
            
            // Fix up Relation to PolymorphicUnion if it points to a union
            match &field.field_type {
                AstFieldType::Relation(target_name) => {
                    if union_symbols.contains(target_name) {
                        field.field_type = AstFieldType::PolymorphicUnion(target_name.clone());
                    } else if !model_symbols.contains(target_name) {
                        return Err(ValidationError(format!(
                            "Field '{}' in model '{}' references unknown type '{}'.",
                            field.name, model.name, target_name
                        )));
                    }
                },
                AstFieldType::RelationArray(target_name) => {
                    if union_symbols.contains(target_name) {
                        return Err(ValidationError(format!(
                            "Field '{}' in model '{}' is an array of union '{}'. Array of unions is currently unsupported.",
                            field.name, model.name, target_name
                        )));
                    } else if !model_symbols.contains(target_name) {
                        return Err(ValidationError(format!(
                            "Field '{}' in model '{}' references unknown type '{}'.",
                            field.name, model.name, target_name
                        )));
                    }
                },
                _ => {}
            }
        }
        
        if id_count != 1 {
            return Err(ValidationError(format!(
                "Model '{}' must have exactly one field with the '@id' attribute, found {}.",
                model.name, id_count
            )));
        }
    }

    // Pass 3: Ambiguous Relations Check
    for model in ast.models.values() {
        let mut target_counts: std::collections::HashMap<&String, Vec<&FieldNode>> = std::collections::HashMap::new();
        
        for field in &model.fields {
            match &field.field_type {
                AstFieldType::Relation(target_name) | AstFieldType::RelationArray(target_name) => {
                    if model_symbols.contains(target_name) {
                        target_counts.entry(target_name).or_insert_with(Vec::new).push(field);
                    }
                },
                _ => {}
            }
        }
        
        for (target_name, relation_fields) in target_counts {
            if relation_fields.len() > 1 {
                let mut seen_names = HashSet::new();
                for field in relation_fields {
                    let mut has_name = false;
                    if let Some(FieldAttribute::Relation { name, .. }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if let Some(n) = name {
                            has_name = true;
                            if !seen_names.insert(n) {
                                return Err(ValidationError(format!(
                                    "Ambiguous relations: Model '{}' has multiple relations to '{}' with the same name '{}'. Each must be uniquely named.",
                                    model.name, target_name, n
                                )));
                            }
                        }
                    }
                    if !has_name {
                        return Err(ValidationError(format!(
                            "Ambiguous relations: Model '{}' has multiple relations to '{}', but field '{}' is missing a unique @relation(\"Name\").",
                            model.name, target_name, field.name
                        )));
                    }
                }
            }
        }
    }

    Ok(ast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_valid_schema() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        assert!(validate_schema(ast).is_ok());
    }

    #[test]
    fn test_missing_id() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "name".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Model 'User' must have exactly one field with the '@id' attribute, found 0.");
    }
    
    #[test]
    fn test_invalid_union_target() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        ast.unions.insert("MyUnion".to_string(), vec!["User".to_string(), "Post".to_string()]);
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Union 'MyUnion' references target 'Post' which does not exist.");
    }

    #[test]
    fn test_multiple_ids() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id1".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "id2".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Model 'User' must have exactly one field with the '@id' attribute, found 2.");
    }

    #[test]
    fn test_invalid_relation_target() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "author".to_string(),
                    field_type: AstFieldType::Relation("UnknownModel".to_string()),
                    attributes: vec![],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Field 'author' in model 'Post' references unknown type 'UnknownModel'.");
    }

    #[test]
    fn test_unsupported_union_array() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Query".to_string(), ModelNode {
            name: "Query".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "results".to_string(),
                    field_type: AstFieldType::RelationArray("SearchResult".to_string()),
                    attributes: vec![],
                }
            ]
        });
        
        ast.unions.insert("SearchResult".to_string(), vec![]);
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Field 'results' in model 'Query' is an array of union 'SearchResult'. Array of unions is currently unsupported.");
    }

    #[test]
    fn test_polymorphic_union_fixup() {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "result".to_string(),
                    field_type: AstFieldType::Relation("SearchResult".to_string()),
                    attributes: vec![],
                }
            ]
        });
        
        ast.unions.insert("SearchResult".to_string(), vec!["Post".to_string()]);
        
        let validated_ast = validate_schema(ast).unwrap();
        let post_model = validated_ast.models.get("Post").unwrap();
        let result_field = post_model.fields.iter().find(|f| f.name == "result").unwrap();
        
        assert_eq!(result_field.field_type, AstFieldType::PolymorphicUnion("SearchResult".to_string()));
    }

    #[test]
    fn test_validation_ambiguous_relations_missing_name() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[]
                reviewedPosts: Post[]
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Ambiguous relations: Model 'User' has multiple relations to 'Post', but field 'authoredPosts' is missing a unique @relation(\"Name\").");
    }

    #[test]
    fn test_validation_ambiguous_relations_duplicate_name() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[] @relation(\"AuthorToPost\")
                reviewedPosts: Post[] @relation(\"AuthorToPost\")
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Ambiguous relations: Model 'User' has multiple relations to 'Post' with the same name 'AuthorToPost'. Each must be uniquely named.");
    }

    #[test]
    fn test_validation_valid_multiple_named_relations() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[] @relation(\"AuthorToPost\")
                reviewedPosts: Post[] @relation(\"ReviewerToPost\")
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_ok());
    }
}
