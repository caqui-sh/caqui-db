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

fn parse_field_def(field_rule: pest::iterators::Pair<Rule>) -> Result<FieldNode, pest::error::Error<Rule>> {
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
                "__id" => attributes.push(FieldAttribute::Id),
                "unique" => attributes.push(FieldAttribute::Unique),
                "track" => attributes.push(FieldAttribute::Track),
                "relation" => {
                    let mut name = None;
                    let mut on_delete = None;
                    
                    if let Some(args_rule) = attr_inner.next() {
                        for param_rule in args_rule.into_inner() {
                            let actual_param = param_rule.into_inner().next().unwrap();
                            if actual_param.as_rule() == Rule::named_arg {
                                let mut param_inner = actual_param.clone().into_inner();
                                let key = param_inner.next().unwrap().as_str();
                                let val_pair = param_inner.next().unwrap();
                                
                                if key == "name" {
                                    let val_rule = val_pair.into_inner().next().unwrap();
                                    if val_rule.as_rule() == Rule::string_lit {
                                        name = Some(val_rule.as_str().trim_matches('"').to_string());
                                    }
                                } else if key == "onDelete" {
                                    let val_rule = val_pair.into_inner().next().unwrap();
                                    on_delete = Some(val_rule.as_str().to_string());
                                } else {
                                    return Err(pest::error::Error::new_from_span(
                                        pest::error::ErrorVariant::CustomError {
                                            message: format!("Unsupported property '{}' in @relation attribute. Use implicit relation bindings instead.", key)
                                        },
                                        actual_param.as_span()
                                    ));
                                }
                            } else if actual_param.as_rule() == Rule::attr_val {
                                let val_rule = actual_param.into_inner().next().unwrap();
                                if val_rule.as_rule() == Rule::string_lit {
                                    name = Some(val_rule.as_str().trim_matches('"').to_string());
                                }
                            }
                        }
                    }
                    attributes.push(FieldAttribute::Relation { name, on_delete });
                },
                _ => {}
            }
        }
    }

    Ok(FieldNode {
        name: field_name,
        field_type: ast_field_type,
        is_optional,
        attributes,
    })
}

pub fn parse_schema(input: &str) -> Result<SchemaAst, pest::error::Error<Rule>> {
    let mut ast = SchemaAst {
        models: HashMap::new(),
        bases: HashMap::new(),
        unions: HashMap::new(),
        enums: HashMap::new(),
    };

    let mut schema_pairs = SchemaParser::parse(Rule::schema, input)?;
    let schema_pair = schema_pairs.next().unwrap();

    for pair in schema_pair.into_inner() {
        match pair.as_rule() {
            Rule::enum_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                let mut variants = Vec::new();
                for inner in inner_rules {
                    if inner.as_rule() == Rule::ident {
                        variants.push(inner.as_str().to_string());
                    }
                }
                ast.enums.insert(name, variants);
            }
            Rule::base_def => {
                let mut inner_rules = pair.into_inner();
                let name = inner_rules.next().unwrap().as_str().to_string();
                
                let mut fields = Vec::new();
                let mut extends = Vec::new();
                let mut block_attributes = Vec::new();
                for inner in inner_rules {
                    match inner.as_rule() {
                        Rule::extends_clause => extends = parse_extends_clause(inner),
                        Rule::field_def => fields.push(parse_field_def(inner)?),
                        Rule::block_attr => {
                            let mut attr_inner = inner.into_inner();
                            let attr_name = attr_inner.next().unwrap().as_str();
                            
                            if attr_name == "id" {
                                let mut default_func = DefaultFunc::AutoIncrement;
                                if let Some(args_pair) = attr_inner.next() {
                                    if args_pair.as_rule() == Rule::attr_args {
                                        if let Some(param) = args_pair.into_inner().next() {
                                            let val_str = param.as_str();
                                            println!("VAL STR IS '{}'", val_str); match val_str {
                                                "uuid()" | "uuid" => default_func = DefaultFunc::Uuid,
                                                "cuid()" | "cuid" => default_func = DefaultFunc::Cuid,
                                                "autoincrement()" | "autoincrement" => default_func = DefaultFunc::AutoIncrement,
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                                block_attributes.push(ModelAttribute::Id(default_func));
                            }
                        }
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
                let mut block_attributes = Vec::new();
                for inner in inner_rules {
                    match inner.as_rule() {
                        Rule::extends_clause => extends = parse_extends_clause(inner),
                        Rule::field_def => fields.push(parse_field_def(inner)?),
                        Rule::block_attr => {
                            let mut attr_inner = inner.into_inner();
                            let attr_name = attr_inner.next().unwrap().as_str();
                            if attr_name == "id" {
                                let mut default_func = DefaultFunc::AutoIncrement;
                                if let Some(args_pair) = attr_inner.next() {
                                    if args_pair.as_rule() == Rule::attr_args {
                                        if let Some(param) = args_pair.into_inner().next() {
                                            let val_str = param.as_str();
                                            match val_str {
                                                "uuid()" | "uuid" => default_func = DefaultFunc::Uuid,
                                                "cuid()" | "cuid" => default_func = DefaultFunc::Cuid,
                                                "autoincrement()" | "autoincrement" => default_func = DefaultFunc::AutoIncrement,
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                                block_attributes.push(ModelAttribute::Id(default_func));
                            } else if attr_name == "track" {
                                block_attributes.push(ModelAttribute::Track);
                            } else if attr_name == "fulltext" {
                                if let Some(args_pair) = attr_inner.next() {
                                    if args_pair.as_rule() == Rule::attr_args {
                                        if let Some(param) = args_pair.into_inner().next() {
                                            if let Some(attr_val) = param.into_inner().next() {
                                                if let Some(array_pair) = attr_val.into_inner().next() {
                                                    if array_pair.as_rule() == Rule::attr_array {
                                                        let fields = array_pair.into_inner().map(|p| p.as_str().to_string()).collect();
                                                        block_attributes.push(ModelAttribute::FullText(fields));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                
                ast.models.insert(name.clone(), ModelNode {
                    name,
                    fields,
                    extends,
                    block_attributes,
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

                name: String
                posts: Post[]
    @@id(uuid)
            }
            model Post {

                title: String
                author: User
    @@id(uuid)
            }
            union SearchResult = User | Post
        ";
        
        let ast = parse_schema(input).unwrap();
        
        let mut expected_ast = SchemaAst {
            models: HashMap::new(),
            bases: HashMap::new(),
            unions: HashMap::new(),
            enums: HashMap::new(),
        };
        
        expected_ast.models.insert("User".to_string(), ModelNode {
            block_attributes: vec![ModelAttribute::Id(DefaultFunc::Uuid)],
            name: "User".to_string(),
            fields: vec![
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
            block_attributes: vec![ModelAttribute::Id(DefaultFunc::Uuid)],
            name: "Post".to_string(),
            fields: vec![
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
                email: String @unique
                bio: String
                posts: Post[] @relation(onDelete: Cascade)
    @@id(uuid)
            }
            model Post {

                authorId: String
    @@id(uuid)
            }
        ";
        
        let ast = parse_schema(input).unwrap();
        let user = ast.models.get("User").unwrap();
        
        // Block attributes
        assert!(user.block_attributes.iter().any(|a| matches!(a, ModelAttribute::Id(_))));
        
        // @unique
        let email_field = user.fields.iter().find(|f| f.name == "email").unwrap();
        assert_eq!(email_field.attributes, vec![FieldAttribute::Unique]);
        
        
        
        
        
        
        
        // @relation
        let posts_field = user.fields.iter().find(|f| f.name == "posts").unwrap();
        assert_eq!(posts_field.attributes, vec![FieldAttribute::Relation {
            name: None,
            on_delete: Some("Cascade".to_string()),
        }]);    }

    #[test]
    fn test_parse_fulltext_attribute() {
        let input = "
            model User {
                title: String
                body: String
                @@fulltext([title, body])
                @@id(uuid)
            }
        ";

        let ast = parse_schema(input).unwrap();
        let user = ast.models.get("User").unwrap();

        let ft = user.block_attributes.iter().find(|a| matches!(a, ModelAttribute::FullText(_))).unwrap();
        if let ModelAttribute::FullText(fields) = ft {
            assert_eq!(fields, &vec!["title".to_string(), "body".to_string()]);
        } else {
            panic!("Expected FullText attribute");
        }
    }

    #[test]
    fn test_parse_relation_names() {
        let input = r#"
            model Post {
                authorId: String
                reviewerId: String
                author: User @relation("AuthorToPost")
                reviewer: User @relation("ReviewerToPost")
                @@id(uuid)
            }
            model User {
                @@id(uuid)
            }
        "#;
        
        let ast = parse_schema(input).unwrap();
        let post = ast.models.get("Post").unwrap();
        
        let author_field = post.fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_field.attributes, vec![FieldAttribute::Relation { 
            name: Some("AuthorToPost".to_string()), 
            on_delete: None, 
        }]);

        let reviewer_field = post.fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert_eq!(reviewer_field.attributes, vec![FieldAttribute::Relation { 
            name: Some("ReviewerToPost".to_string()), 
            on_delete: None, 
        }]);
    }

    #[test]
    fn test_parse_optional_fields() {
        let input = "
            model User {

                bio: String?
                manager: User? @relation
                managerId: String?
    @@id(uuid)
            }
        ";
        
        let ast = parse_schema(input).unwrap();
        let user = ast.models.get("User").unwrap();
        
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
            base A { id: String  }
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
            base Empty { }
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
    fn test_parse_relation_unsupported_properties() {
        let input_fields = r#"
            model User {
                posts: Post[] @relation(fields: [__id])
            }
        "#;
        let result_fields = parse_schema(input_fields);
        assert!(result_fields.is_err());
        assert!(result_fields.unwrap_err().to_string().contains("Unsupported property 'fields' in @relation attribute. Use implicit relation bindings instead."));
        
        let input_references = r#"
            model User {
                posts: Post[] @relation(references: [authorId])
            }
        "#;
        let result_references = parse_schema(input_references);
        assert!(result_references.is_err());
        assert!(result_references.unwrap_err().to_string().contains("Unsupported property 'references' in @relation attribute. Use implicit relation bindings instead."));

        let input_mixed_name = r#"
            model User {
                posts: Post[] @relation("AuthorToPost", fields: [authorId], references: [__id])
            }
        "#;
        let result_mixed_name = parse_schema(input_mixed_name);
        assert!(result_mixed_name.is_err());
        // Since attributes are processed in order, we expect 'fields' error to hit first.
        assert!(result_mixed_name.unwrap_err().to_string().contains("Unsupported property 'fields'"));

        let input_mixed_on_delete = r#"
            model User {
                posts: Post[] @relation(fields: [authorId], references: [__id], onDelete: Cascade)
            }
        "#;
        let result_mixed_on_delete = parse_schema(input_mixed_on_delete);
        assert!(result_mixed_on_delete.is_err());
        assert!(result_mixed_on_delete.unwrap_err().to_string().contains("Unsupported property 'fields'"));

        let input_kitchen_sink = r#"
            model User {
                posts: Post[] @relation("AuthorToPost", fields: [authorId], references: [__id], onDelete: Cascade)
            }
        "#;
        let result_kitchen_sink = parse_schema(input_kitchen_sink);
        assert!(result_kitchen_sink.is_err());
        assert!(result_kitchen_sink.unwrap_err().to_string().contains("Unsupported property 'fields'"));
    }
}
