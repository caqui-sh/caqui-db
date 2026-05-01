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
                if field.is_optional {
                    return Err(ValidationError(format!(
                        "Field '{}' in model '{}' is marked with '@id' but is optional. Primary keys cannot be optional.",
                        field.name, model.name
                    )));
                }
                id_count += 1;
            }

            // Check if arrays are optional
            let is_array = matches!(field.field_type, AstFieldType::ScalarArray(_) | AstFieldType::RelationArray(_));
            if is_array && field.is_optional {
                return Err(ValidationError(format!(
                    "Field '{}' in model '{}' is an array but is marked as optional. Arrays cannot be optional.",
                    field.name, model.name
                )));
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
                        field.field_type = AstFieldType::PolymorphicUnionArray(target_name.clone());
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

    // Pass 4: Implicit Foreign Key Injection
    let mut model_id_info = std::collections::HashMap::new();
    for model in ast.models.values() {
        if let Some(id_field) = model.fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))) {
            model_id_info.insert(model.name.clone(), (id_field.name.clone(), id_field.field_type.clone()));
        }
    }

    for model in ast.models.values_mut() {
        let mut added_fields = Vec::new();
        
        for field in &mut model.fields {
            if let AstFieldType::Relation(target_name) = &field.field_type {
                let mut relation_attr_idx = None;
                let mut needs_injection = false;
                let mut col_name = format!("{}Id", field.name);
                
                for (idx, attr) in field.attributes.iter().enumerate() {
                    if let FieldAttribute::Relation { fields, column, .. } = attr {
                        relation_attr_idx = Some(idx);
                        if fields.is_empty() {
                            needs_injection = true;
                            if let Some(c) = column {
                                col_name = c.clone();
                            }
                        }
                        break;
                    }
                }
                
                if relation_attr_idx.is_none() {
                    needs_injection = true;
                    field.attributes.push(FieldAttribute::Relation {
                        name: None,
                        fields: vec![],
                        references: vec![],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    });
                    relation_attr_idx = Some(field.attributes.len() - 1);
                }

                if needs_injection {
                    if let Some((target_id_name, target_id_type)) = model_id_info.get(target_name) {
                        added_fields.push(FieldNode {
                            name: col_name.clone(),
                            field_type: target_id_type.clone(),
                            is_optional: field.is_optional,
                            attributes: vec![],
                        });
                        
                        if let Some(idx) = relation_attr_idx {
                            if let FieldAttribute::Relation { fields, references, .. } = &mut field.attributes[idx] {
                                *fields = vec![col_name.clone()];
                                *references = vec![target_id_name.clone()];
                            }
                        }
                    } else {
                        return Err(ValidationError(format!("Target model '{}' does not have an @id field.", target_name)));
                    }
                }
            }
        }
        
        for added_field in added_fields {
            if !model.fields.iter().any(|f| f.name == added_field.name) {
                model.fields.push(added_field);
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
                    is_optional: false,
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
                    is_optional: false,
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
                    is_optional: false,
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
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "id2".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
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
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "author".to_string(),
                    field_type: AstFieldType::Relation("UnknownModel".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Field 'author' in model 'Post' references unknown type 'UnknownModel'.");
    }

    #[test]
    fn test_union_array_fixup() {
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
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "results".to_string(),
                    field_type: AstFieldType::RelationArray("SearchResult".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });

        ast.unions.insert("SearchResult".to_string(), vec![]);

        let result = validate_schema(ast).unwrap();
        let query = result.models.get("Query").unwrap();
        let results_field = query.fields.iter().find(|f| f.name == "results").unwrap();
        assert_eq!(results_field.field_type, AstFieldType::PolymorphicUnionArray("SearchResult".to_string()));
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
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "result".to_string(),
                    field_type: AstFieldType::Relation("SearchResult".to_string()),
                    is_optional: false,
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

    #[test]
    fn test_implicit_foreign_key_injection() {
        let input = "
            model User {
                id: String @id
            }
            model Post {
                id: String @id
                author: User @relation(\"AuthorToPost\")
                reviewer: User @relation(\"ReviewerToPost\", column: \"reviewer_id\")
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let post = ast.models.get("Post").unwrap();
        
        let author_id_field = post.fields.iter().find(|f| f.name == "authorId").unwrap();
        assert_eq!(author_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let reviewer_id_field = post.fields.iter().find(|f| f.name == "reviewer_id").unwrap();
        assert_eq!(reviewer_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let author_rel = post.fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("AuthorToPost".to_string()),
            fields: vec!["authorId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);

        let reviewer_rel = post.fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert_eq!(reviewer_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("ReviewerToPost".to_string()),
            fields: vec!["reviewer_id".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: Some("reviewer_id".to_string()),
        }]);
    }

    #[test]
    fn test_implicit_relation_explicit_bypass() {
        let input = "
            model User {
                id: String @id
            }
            model Post {
                id: String @id
                authorId: String
                author: User @relation(fields: [authorId], references: [id])
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let post = ast.models.get("Post").unwrap();
        assert_eq!(post.fields.len(), 3);
        
        let author_rel = post.fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_rel.attributes, vec![FieldAttribute::Relation {
            name: None,
            fields: vec!["authorId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_implicit_relation_strict_type_matching() {
        let input = "
            model Category {
                id: Int @id
            }
            model Product {
                id: String @id
                category: Category
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let product = ast.models.get("Product").unwrap();
        let cat_id_field = product.fields.iter().find(|f| f.name == "categoryId").unwrap();
        assert_eq!(cat_id_field.field_type, AstFieldType::Scalar("Int".to_string()));
    }

    #[test]
    fn test_implicit_relation_missing_target_id() {
        // Here Author lacks an @id field
        let input = "
            model Author {
                name: String
            }
            model Book {
                id: String @id
                author: Author
            }
        ";
        // Parse the schema. Note: Pass 2 normally catches missing ID for Author itself.
        // Wait, Pass 2 will fail first: "Model 'Author' must have exactly one field with the '@id' attribute, found 0."
        // That is totally fine and correct. Let's just assert that it fails.
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
    }

    #[test]
    fn test_implicit_relation_self_referential() {
        let input = "
            model Employee {
                id: String @id
                manager: Employee @relation(\"Management\")
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let employee = ast.models.get("Employee").unwrap();
        let manager_id_field = employee.fields.iter().find(|f| f.name == "managerId").unwrap();
        assert_eq!(manager_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let manager_rel = employee.fields.iter().find(|f| f.name == "manager").unwrap();
        assert_eq!(manager_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("Management".to_string()),
            fields: vec!["managerId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_validation_optional_id_fails() {
        let input = "
            model User {
                id: String? @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_err());
        assert!(res.unwrap_err().0.contains("Primary keys cannot be optional"));
    }

    #[test]
    fn test_validation_optional_array_fails() {
        let input = "
            model User {
                id: String @id
                tags: String[]?
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_err());
        assert!(res.unwrap_err().0.contains("Arrays cannot be optional"));
    }

    #[test]
    fn test_validation_implicit_fk_inherits_optionality() {
        let input = "
            model User {
                id: String @id
                manager: User?
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast).unwrap();
        let user = res.models.get("User").unwrap();
        let manager_id = user.fields.iter().find(|f| f.name == "managerId").unwrap();
        assert_eq!(manager_id.is_optional, true);
        }

        #[test]
        fn test_parse_and_validate_union_array() {
        let input = "
            model Query {
                id: String @id
                results: SearchResult[]
            }
            model User {
                id: String @id
            }
            model Post {
                id: String @id
            }
            union SearchResult = User | Post
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast).unwrap();

        let query = res.models.get("Query").unwrap();
        let results_field = query.fields.iter().find(|f| f.name == "results").unwrap();
        assert_eq!(results_field.field_type, AstFieldType::PolymorphicUnionArray("SearchResult".to_string()));
        }

        #[test]
        fn test_validation_optional_unique_succeeds() {
        let input = "
            model User {
                id: String @id
                email: String? @unique
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_ok());
    }
}
