pub mod differ;
pub mod introspection;
pub mod migration;
pub mod workflows;

use schema_parser::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalColumn {
    pub name: String,
    pub sqlite_type: String, // TEXT, INTEGER, REAL, BLOB
    pub is_json_array: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalTable {
    pub name: String,
    pub columns: Vec<PhysicalColumn>,
}

pub fn lower_ast_to_physical(ast: &SchemaAst) -> Vec<PhysicalTable> {
    let mut physical_tables = Vec::new();

    // Iterate sorted to ensure deterministic output for testing/diffing
    let mut models: Vec<_> = ast.models.values().collect();
    models.sort_by_key(|m| &m.name);

    for model in models {
        let mut columns = Vec::new();

        for field in &model.fields {
            match &field.field_type {
                AstFieldType::ScalarArray(_) | AstFieldType::RelationArray(_) => {
                    columns.push(PhysicalColumn {
                        name: field.name.clone(),
                        sqlite_type: "TEXT".to_string(), // Tagged internally for JSON1
                        is_json_array: true,             
                    });
                },
                AstFieldType::PolymorphicUnion(_) => {
                    // Drop original field; inject discriminator string and ID pointer
                    columns.push(PhysicalColumn {
                        name: format!("{}_type", field.name),
                        sqlite_type: "TEXT".to_string(),
                        is_json_array: false,
                    });
                    columns.push(PhysicalColumn {
                        name: format!("{}_id", field.name),
                        sqlite_type: "TEXT".to_string(), // Or INTEGER depending on PK definition, assuming TEXT
                        is_json_array: false,
                    });
                },
                AstFieldType::Scalar(t) => {
                    let sql_type = match t.as_str() {
                        "Int" => "INTEGER",
                        "Float" => "REAL",
                        "Boolean" => "INTEGER",
                        _ => "TEXT",
                    };
                    columns.push(PhysicalColumn {
                        name: field.name.clone(),
                        sqlite_type: sql_type.to_string(),
                        is_json_array: false,
                    });
                },
                AstFieldType::Relation(_) => {
                    // Standard relations omitted for brevity as per phase 2 constraints
                }
            }
        }
        physical_tables.push(PhysicalTable { name: model.name.clone(), columns });
    }
    physical_tables
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_lower_ast_to_physical() {
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
                    attributes: vec!["@id".to_string()],
                },
                FieldNode {
                    name: "tags".to_string(),
                    field_type: AstFieldType::ScalarArray("String".to_string()),
                    attributes: vec![],
                },
                FieldNode {
                    name: "search".to_string(),
                    field_type: AstFieldType::PolymorphicUnion("SearchResult".to_string()),
                    attributes: vec![],
                },
                FieldNode {
                    name: "age".to_string(),
                    field_type: AstFieldType::Scalar("Int".to_string()),
                    attributes: vec![],
                },
            ]
        });
        
        let tables = lower_ast_to_physical(&ast);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.name, "User");
        assert_eq!(table.columns.len(), 5);
        
        let id_col = table.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id_col.sqlite_type, "TEXT");
        assert_eq!(id_col.is_json_array, false);
        
        let tags_col = table.columns.iter().find(|c| c.name == "tags").unwrap();
        assert_eq!(tags_col.sqlite_type, "TEXT");
        assert_eq!(tags_col.is_json_array, true);
        
        let search_type_col = table.columns.iter().find(|c| c.name == "search_type").unwrap();
        assert_eq!(search_type_col.sqlite_type, "TEXT");
        
        let search_id_col = table.columns.iter().find(|c| c.name == "search_id").unwrap();
        assert_eq!(search_id_col.sqlite_type, "TEXT");
        
        let age_col = table.columns.iter().find(|c| c.name == "age").unwrap();
        assert_eq!(age_col.sqlite_type, "INTEGER");
    }
}
