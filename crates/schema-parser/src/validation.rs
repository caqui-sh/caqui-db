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
}
