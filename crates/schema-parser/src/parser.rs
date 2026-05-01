use pest::Parser;
use std::collections::{HashMap, BTreeSet};
use crate::ast::*;

#[derive(pest_derive::Parser)]
#[grammar = "schema.pest"]
pub struct SchemaParser;

fn parse_extends_clause(pair: pest::iterators::Pair<Rule>) -> Vec<String> {
    debug_assert_eq!(pair.as_rule(), Rule::extends_clause);
    let mut extends = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::ident {
            extends.push(inner.as_str().to_string());
        }
    }
    extends
}

fn parse_field_def(field_rule: pest::iterators::Pair<Rule>) -> FieldNode {
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

    FieldNode {
        name: field_name,
        field_type: ast_field_type,
        is_optional,
        attributes,
    }
}

pub fn parse_schema(input: &str) -> Result<SchemaAst, pest::error::Error<Rule>> {
    let mut ast = SchemaAst {
        models: HashMap::new(),
        bases: HashMap::new(),
        unions: HashMap::new(),
    };

    let mut schema_pairs = SchemaParser::parse(Rule::schema, input)?;
    let schema_pair = schema_pairs.next().unwrap();

    for pair in schema_pair.into_inner() {
        match pair.as_rule() {
            Rule::base_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                
                let mut fields = Vec::new();
                let mut extends = Vec::new();
                for inner in inner_rules {
                    match inner.as_rule() {
                        Rule::extends_clause => extends = parse_extends_clause(inner),
                        Rule::field_def => fields.push(parse_field_def(inner)),
                        _ => {}
                    }
                }
                
                ast.bases.insert(name.clone(), BaseNode {
                    name,
                    fields,
                    extends,
                    resolved_fields: Vec::new(),
                    resolved_bases: BTreeSet::new(),
                });
            },
            Rule::model_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                
                let mut fields = Vec::new();
                let mut extends = Vec::new();
                for inner in inner_rules {
                    match inner.as_rule() {
                        Rule::extends_clause => extends = parse_extends_clause(inner),
                        Rule::field_def => fields.push(parse_field_def(inner)),
                        _ => {}
                    }
                }
                
                ast.models.insert(name.clone(), ModelNode {
                    name,
                    fields,
                    extends,
                    resolved_fields: Vec::new(),
                    resolved_bases: BTreeSet::new(),
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
            bases: HashMap::new(),
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
            extends: vec![],
            resolved_fields: vec![],
            resolved_bases: BTreeSet::new(),
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
            extends: vec![],
            resolved_fields: vec![],
            resolved_bases: BTreeSet::new(),
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

    #[test]
    fn test_parse_base_and_model_inheritance() {
        let schema_str = r#"
            base Timestamped {
                createdAt: DateTime
            }
            base Record extends Timestamped {
                id: String @id
            }
            model User extends Record, Timestamped {
                name: String
            }
        "#;
        
        let schema = parse_schema(schema_str).expect("Failed to parse schema");
        
        // 1. Assert Bases captured correctly
        assert_eq!(schema.bases.len(), 2);
        let timestamped = schema.bases.get("Timestamped").unwrap();
        assert!(timestamped.extends.is_empty());
        assert_eq!(timestamped.fields[0].name, "createdAt");
        
        let record = schema.bases.get("Record").unwrap();
        assert_eq!(record.extends, vec!["Timestamped"]);
        
        // 2. Assert Model captured extends clause correctly
        assert_eq!(schema.models.len(), 1);
        let user = schema.models.get("User").unwrap();
        assert_eq!(user.name, "User");
        assert_eq!(user.extends, vec!["Record", "Timestamped"]);
        
        // 3. Assert Phase 2 compiler states are cleanly un-initialized
        assert!(user.resolved_bases.is_empty());
        assert!(user.resolved_fields.is_empty());
    }

    #[test]
    fn test_parse_base_trailing_commas_and_whitespace() {
        let schema_str = r#"
            base A { id: String }
            base B extends A, {
                name: String
            }
            model C extends 
                A, 
                B, 
            {
                age: Int
            }
        "#;
        
        let schema = parse_schema(schema_str).expect("Failed to parse trailing commas and whitespace");
        assert_eq!(schema.bases.get("B").unwrap().extends, vec!["A"]);
        assert_eq!(schema.models.get("C").unwrap().extends, vec!["A", "B"]);
    }

    #[test]
    fn test_parse_empty_base_and_single_inheritance() {
        let schema_str = r#"
            base Empty {}
            base Single extends Empty {
                id: String
            }
        "#;
        
        let schema = parse_schema(schema_str).expect("Failed to parse empty base");
        let empty = schema.bases.get("Empty").unwrap();
        assert!(empty.fields.is_empty());
        assert!(empty.extends.is_empty());
        
        let single = schema.bases.get("Single").unwrap();
        assert_eq!(single.extends, vec!["Empty"]);
        assert_eq!(single.fields.len(), 1);
    }

    #[test]
    fn test_parse_base_negative_missing_identifier() {
        let schema_str = r#"
            model User extends {
                id: String
            }
        "#;
        assert!(parse_schema(schema_str).is_err());
    }

    #[test]
    fn test_parse_base_negative_double_commas() {
        let schema_str = r#"
            model User extends Record,, Timestamped {
                id: String
            }
        "#;
        assert!(parse_schema(schema_str).is_err());
    }

    #[test]
    fn test_parse_base_negative_block_attributes() {
        // block_attr `@@index` is valid on models, but deliberately omitted from `base_def`
        let schema_str = r#"
            base InvalidBase {
                id: String
                @@index([id])
            }
        "#;
        assert!(parse_schema(schema_str).is_err());
    }
}
