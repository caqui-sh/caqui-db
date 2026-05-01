use pest::Parser;
use std::collections::HashMap;
use crate::ast::*;

#[derive(pest_derive::Parser)]
#[grammar = "schema.pest"]
pub struct SchemaParser;

pub fn parse_schema(input: &str) -> Result<SchemaAst, pest::error::Error<Rule>> {
    let mut ast = SchemaAst {
        models: HashMap::new(),
        unions: HashMap::new(),
    };

    let mut schema_pairs = SchemaParser::parse(Rule::schema, input)?;
    let schema_pair = schema_pairs.next().unwrap();

    for pair in schema_pair.into_inner() {
        match pair.as_rule() {
            Rule::model_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                
                let mut fields = Vec::new();
                for field_rule in inner_rules {
                    if field_rule.as_rule() == Rule::field_def {
                        let mut field_inner = field_rule.into_inner();
                        let field_name = field_inner.next().unwrap().as_str().to_string();
                        let field_type_rule = field_inner.next().unwrap();
                        
                        let mut ft_inner = field_type_rule.into_inner();
                        let type_ident_rule = ft_inner.next().unwrap();
                        let is_scalar = type_ident_rule.as_rule() == Rule::scalar;
                        let type_name = type_ident_rule.as_str().to_string();

                        let mut is_array = false;
                        let mut is_optional = false;

                        for sub_pair in ft_inner {
                            match sub_pair.as_rule() {
                                Rule::is_array => is_array = true,
                                Rule::is_optional => is_optional = true,
                                _ => {}
                            }
                        }
                        
                        let ast_field_type = if is_scalar {
                            if is_array {
                                AstFieldType::ScalarArray(type_name)
                            } else {
                                AstFieldType::Scalar(type_name)
                            }
                        } else {
                            // We don't know if this custom type is a Relation or PolymorphicUnion yet.
                            // We assume it's a Relation for now, and the validation pass will correct it.
                            if is_array {
                                AstFieldType::RelationArray(type_name)
                            } else {
                                AstFieldType::Relation(type_name)
                            }
                        };
                        
                        let mut attributes = Vec::new();
                        for attr_rule in field_inner {
                            if attr_rule.as_rule() == Rule::field_attr {
                                let mut attr_inner = attr_rule.into_inner();
                                let attr_ident = attr_inner.next().unwrap().as_str();
                                
                                match attr_ident {
                                    "id" => attributes.push(FieldAttribute::Id),
                                    "unique" => attributes.push(FieldAttribute::Unique),
                                    "updatedAt" => attributes.push(FieldAttribute::UpdatedAt),
                                    "ignore" => attributes.push(FieldAttribute::Ignore),
                                    "map" => {
                                        if let Some(args_rule) = attr_inner.next() {
                                            let arg_val = args_rule.into_inner().next().unwrap().into_inner().next().unwrap().as_str();
                                            let clean_val = arg_val.trim_matches('"').to_string();
                                            attributes.push(FieldAttribute::Map(clean_val));
                                        }
                                    },
                                    "default" => {
                                        if let Some(args_rule) = attr_inner.next() {
                                            let arg_val = args_rule.into_inner().next().unwrap().into_inner().next().unwrap().as_str();
                                            let default_func = match arg_val {
                                                "autoincrement()" => DefaultFunc::AutoIncrement,
                                                "now()" => DefaultFunc::Now,
                                                "uuid()" => DefaultFunc::Uuid,
                                                "cuid()" => DefaultFunc::Cuid,
                                                _ => DefaultFunc::Static(arg_val.trim_matches('"').to_string()),
                                            };
                                            attributes.push(FieldAttribute::Default(default_func));
                                        }
                                    },
                                    "relation" => {
                                        let mut name = None;
                                        let mut fields_vec = Vec::new();
                                        let mut refs_vec = Vec::new();
                                        let mut on_delete = None;
                                        let mut deferrable = false;
                                        let mut column = None;
                                        
                                        if let Some(args_rule) = attr_inner.next() {
                                            for param_rule in args_rule.into_inner() {
                                                // attr_param -> named_arg or attr_val
                                                let actual_param = param_rule.into_inner().next().unwrap();
                                                if actual_param.as_rule() == Rule::named_arg {
                                                    let mut param_inner = actual_param.into_inner();
                                                    let key = param_inner.next().unwrap().as_str();
                                                    let val_pair = param_inner.next().unwrap();
                                                    
                                                    if key == "fields" {
                                                        let val_rule = val_pair.into_inner().next().unwrap();
                                                        if val_rule.as_rule() == Rule::attr_array {
                                                            fields_vec = val_rule.into_inner().map(|r| r.as_str().to_string()).collect();
                                                        }
                                                    } else if key == "references" {
                                                        let val_rule = val_pair.into_inner().next().unwrap();
                                                        if val_rule.as_rule() == Rule::attr_array {
                                                            refs_vec = val_rule.into_inner().map(|r| r.as_str().to_string()).collect();
                                                        }
                                                    } else if key == "onDelete" {
                                                        let val_rule = val_pair.into_inner().next().unwrap();
                                                        on_delete = Some(val_rule.as_str().to_string());
                                                    } else if key == "deferrable" {
                                                        if val_pair.as_str() == "true" {
                                                            deferrable = true;
                                                        }
                                                    } else if key == "column" {
                                                        let val_rule = val_pair.into_inner().next().unwrap();
                                                        column = Some(val_rule.as_str().trim_matches('"').to_string());
                                                    }
                                                } else if actual_param.as_rule() == Rule::attr_val {
                                                    let val_rule = actual_param.into_inner().next().unwrap();
                                                    if val_rule.as_rule() == Rule::string_lit {
                                                        name = Some(val_rule.as_str().trim_matches('"').to_string());
                                                    }
                                                }
                                            }
                                        }
                                        attributes.push(FieldAttribute::Relation { name, fields: fields_vec, references: refs_vec, on_delete, deferrable, column });
                                    },
                                    _ => {}
                                }
                            }
                        }

                        fields.push(FieldNode {
                            name: field_name,
                            field_type: ast_field_type,
                            is_optional,
                            attributes,
                        });
                    }
                }
                
                ast.models.insert(name.clone(), ModelNode {
                    name,
                    fields,
                });
            },
            Rule::union_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                
                let mut targets = Vec::new();
                for target_rule in inner_rules {
                    targets.push(target_rule.as_str().to_string());
                }
                
                ast.unions.insert(name, targets);
            },
            Rule::EOI => (),
            rule => panic!("Unexpected rule: {:?}", rule),
        }
    }

    Ok(ast)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schema() {
        let input = "
            model User {
                id: String @id
                name: String
                posts: Post[]
            }
            model Post {
                id: String @id
                title: String
                author: User
            }
            union SearchResult = User | Post
        ";
        
        let ast = parse_schema(input).unwrap();
        
        let mut expected_ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        expected_ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "name".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "posts".to_string(),
                    field_type: AstFieldType::RelationArray("Post".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
            ],
        });
        
        expected_ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "title".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "author".to_string(),
                    field_type: AstFieldType::Relation("User".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
            ],
        });
        
        expected_ast.unions.insert("SearchResult".to_string(), vec!["User".to_string(), "Post".to_string()]);
        
        assert_eq!(ast, expected_ast);
    }

    #[test]
    fn test_parse_advanced_attributes() {
        let input = "
            model User {
                id: String @id @map(\"user_id\")
                email: String @unique
                bio: String @default(\"no bio\")
                createdAt: DateTime @default(now())
                token: String @ignore
                posts: Post[] @relation(fields: [id], references: [authorId], onDelete: Cascade)
            }
            model Post {
                id: String @id
                authorId: String
            }
        ";
        
        let ast = parse_schema(input).unwrap();
        let user = ast.models.get("User").unwrap();
        
        // @map
        let id_field = user.fields.iter().find(|f| f.name == "id").unwrap();
        assert_eq!(id_field.attributes, vec![FieldAttribute::Id, FieldAttribute::Map("user_id".to_string())]);
        
        // @unique
        let email_field = user.fields.iter().find(|f| f.name == "email").unwrap();
        assert_eq!(email_field.attributes, vec![FieldAttribute::Unique]);
        
        // @default("string")
        let bio_field = user.fields.iter().find(|f| f.name == "bio").unwrap();
        assert_eq!(bio_field.attributes, vec![FieldAttribute::Default(DefaultFunc::Static("no bio".to_string()))]);
        
        // @default(now())
        let created_at_field = user.fields.iter().find(|f| f.name == "createdAt").unwrap();
        assert_eq!(created_at_field.attributes, vec![FieldAttribute::Default(DefaultFunc::Now)]);
        
        // @ignore
        let token_field = user.fields.iter().find(|f| f.name == "token").unwrap();
        assert_eq!(token_field.attributes, vec![FieldAttribute::Ignore]);
        
        // @relation
        let posts_field = user.fields.iter().find(|f| f.name == "posts").unwrap();
        assert_eq!(posts_field.attributes, vec![FieldAttribute::Relation {
            name: None,
            fields: vec!["id".to_string()],
            references: vec!["authorId".to_string()],
            on_delete: Some("Cascade".to_string()),
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_parse_relation_names() {
        let input = "
            model Post {
                id: String @id
                authorId: String
                reviewerId: String
                author: User @relation(\"AuthorToPost\", fields: [authorId], references: [id])
                reviewer: User @relation(\"ReviewerToPost\", fields: [reviewerId], references: [id])
            }
            model User {
                id: String @id
            }
        ";
        
        let ast = parse_schema(input).unwrap();
        let post = ast.models.get("Post").unwrap();
        
        let author_field = post.fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_field.attributes, vec![FieldAttribute::Relation {
            name: Some("AuthorToPost".to_string()),
            fields: vec!["authorId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);

        let reviewer_field = post.fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert_eq!(reviewer_field.attributes, vec![FieldAttribute::Relation {
            name: Some("ReviewerToPost".to_string()),
            fields: vec!["reviewerId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_parse_optional_fields() {
        let input = "
            model User {
                id: String @id
                bio: String?
                manager: User? @relation(fields: [managerId], references: [id])
                managerId: String?
            }
        ";
        
        let ast = parse_schema(input).unwrap();
        let user = ast.models.get("User").unwrap();
        
        let id_field = user.fields.iter().find(|f| f.name == "id").unwrap();
        assert!(!id_field.is_optional);
        
        let bio_field = user.fields.iter().find(|f| f.name == "bio").unwrap();
        assert!(bio_field.is_optional);
        assert_eq!(bio_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let manager_field = user.fields.iter().find(|f| f.name == "manager").unwrap();
        assert!(manager_field.is_optional);
        assert_eq!(manager_field.field_type, AstFieldType::Relation("User".to_string()));
        
        let manager_id_field = user.fields.iter().find(|f| f.name == "managerId").unwrap();
        assert!(manager_id_field.is_optional);
        assert_eq!(manager_id_field.field_type, AstFieldType::Scalar("String".to_string()));
    }
}
