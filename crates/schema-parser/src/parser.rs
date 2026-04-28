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
                        let is_array = ft_inner.next().map(|r| r.as_rule() == Rule::is_array).is_some();
                        
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
                            attributes.push(attr_rule.as_str().trim().to_string());
                        }

                        fields.push(FieldNode {
                            name: field_name,
                            field_type: ast_field_type,
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
                id String @id
                name String
                posts Post[]
            }
            model Post {
                id String @id
                title String
                author User
            }
            union SearchResult = User | Post
        ";
        
        let ast = parse_schema(input).unwrap();
        assert_eq!(ast.models.len(), 2);
        assert_eq!(ast.unions.len(), 1);
        
        let user_model = ast.models.get("User").unwrap();
        assert_eq!(user_model.fields.len(), 3);
        assert_eq!(user_model.fields[0].name, "id");
        assert_eq!(user_model.fields[0].attributes, vec!["@id"]);
        assert_eq!(user_model.fields[0].field_type, AstFieldType::Scalar("String".to_string()));
        
        assert_eq!(user_model.fields[2].name, "posts");
        assert_eq!(user_model.fields[2].field_type, AstFieldType::RelationArray("Post".to_string()));
        
        let search_result = ast.unions.get("SearchResult").unwrap();
        assert_eq!(search_result, &vec!["User".to_string(), "Post".to_string()]);
    }
}
