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
pub struct PhysicalIndex {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalTrigger {
    pub name: String,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalTable {
    pub name: String,
    pub columns: Vec<PhysicalColumn>,
    pub indexes: Vec<PhysicalIndex>,
    pub triggers: Vec<PhysicalTrigger>,
    pub foreign_keys: Vec<String>,
}

pub fn lower_ast_to_physical(ast: &SchemaAst) -> Vec<PhysicalTable> {
    let mut physical_tables = Vec::new();

    // Iterate sorted to ensure deterministic output for testing/diffing
    let mut models: Vec<_> = ast.models.values().collect();
    models.sort_by_key(|m| &m.name);

    for model in models {
        let mut columns = Vec::new();
        let mut indexes = Vec::new();
        let mut triggers = Vec::new();
        let mut foreign_keys = Vec::new();

        for field in &model.resolved_fields {
            let is_unique = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Unique));
            if is_unique {
                indexes.push(PhysicalIndex {
                    name: format!("idx_{}_{}", model.name, field.name),
                    columns: vec![field.name.clone()],
                    unique: true,
                });
            }

            let is_updated_at = field.attributes.iter().any(|a| matches!(a, FieldAttribute::UpdatedAt));
            if is_updated_at {
                let trigger_name = format!("trg_update_{}_{}", model.name, field.name);
                let sql = format!(
                    "CREATE TRIGGER IF NOT EXISTS {} \n\
                     AFTER UPDATE ON {} \n\
                     FOR EACH ROW \n\
                     WHEN OLD.{} IS NULL OR NEW.{} <= OLD.{} \n\
                     BEGIN \n\
                         UPDATE {} SET {} = CURRENT_TIMESTAMP WHERE id = OLD.id; \n\
                     END;",
                    trigger_name, model.name, field.name, field.name, field.name, model.name, field.name
                );
                triggers.push(PhysicalTrigger {
                    name: trigger_name,
                    sql,
                });
            }

            match &field.field_type {
                AstFieldType::ScalarArray(_) | AstFieldType::RelationArray(_) | AstFieldType::PolymorphicUnionArray(_) => {
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
                    let is_id = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id));
                    let is_autoincrement = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Default(DefaultFunc::AutoIncrement)));
                    let is_uuid = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Default(DefaultFunc::Uuid)));
                    
                    let mut sql_type = match t.as_str() {
                        "Int" => "INTEGER",
                        "Float" => "REAL",
                        "Boolean" => "INTEGER",
                        _ => "TEXT",
                    }.to_string();
                    
                    if is_id {
                        if sql_type == "INTEGER" && is_autoincrement {
                            sql_type = "INTEGER PRIMARY KEY AUTOINCREMENT".to_string();
                        } else {
                            sql_type = format!("{} PRIMARY KEY", sql_type);
                        }
                        
                        if is_uuid {
                            sql_type = format!("{} DEFAULT (gen_uuid7())", sql_type);
                        }
                    } else if field.name.starts_with("__") {
                        if let Some(FieldAttribute::Default(DefaultFunc::Static(val))) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Default(DefaultFunc::Static(_)))) {
                            if sql_type == "INTEGER" && val == "true" {
                                sql_type = format!("{} DEFAULT 1 NOT NULL", sql_type);
                            } else if sql_type == "INTEGER" && val == "false" {
                                sql_type = format!("{} DEFAULT 0 NOT NULL", sql_type);
                            } else {
                                sql_type = format!("{} DEFAULT '{}' NOT NULL", sql_type, val);
                            }
                        }
                    }

                    columns.push(PhysicalColumn {
                        name: field.name.clone(),
                        sqlite_type: sql_type,
                        is_json_array: false,
                    });
                },
                AstFieldType::Relation(ref_model) => {
                    // Skip foreign key generation if the target is an abstract base
                    let is_base_target = ast.bases.contains_key(ref_model);

                    if !is_base_target {
                        // Map @relation attributes to physical FOREIGN KEY definitions
                        if let Some(FieldAttribute::Relation { name: _, fields, references, on_delete, deferrable, .. }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                            if !fields.is_empty() && !references.is_empty() {
                                let mut fk_def = format!("FOREIGN KEY ({}) REFERENCES \"{}\" ({})", fields.join(", "), ref_model, references.join(", "));
                                
                                if let Some(action) = on_delete {
                                    let sql_action = match action.to_uppercase().as_str() {
                                        "CASCADE" => "CASCADE",
                                        "SETNULL" => "SET NULL",
                                        "SETDEFAULT" => "SET DEFAULT",
                                        "RESTRICT" => "RESTRICT",
                                        _ => "NO ACTION" // default fallback
                                    };
                                    fk_def.push_str(&format!(" ON DELETE {}", sql_action));
                                }

                                if *deferrable {
                                    fk_def.push_str(" DEFERRABLE INITIALLY DEFERRED");
                                }
                                
                                foreign_keys.push(fk_def);
                            }
                        }
                    }
                }
            }
        }
        physical_tables.push(PhysicalTable { name: model.name.clone(), columns, indexes, triggers, foreign_keys });
    }
    physical_tables
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_lower_ast_to_physical() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode { extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "tags".to_string(),
                    field_type: AstFieldType::ScalarArray("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "search".to_string(),
                    field_type: AstFieldType::PolymorphicUnion("SearchResult".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "age".to_string(),
                    field_type: AstFieldType::Scalar("Int".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
            ]
        });
        
        let tables = lower_ast_to_physical(&ast);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.name, "User");
        assert_eq!(table.columns.len(), 5);
        assert_eq!(table.indexes.len(), 0);
        assert_eq!(table.triggers.len(), 0);
        
        let id_col = table.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id_col.sqlite_type, "TEXT PRIMARY KEY");
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

    #[test]
    fn test_lower_ast_to_physical_advanced() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Device".to_string(), ModelNode { extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Device".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)],
                },
                FieldNode {
                    name: "serial".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Unique],
                },
                FieldNode {
                    name: "updated_at".to_string(),
                    field_type: AstFieldType::Scalar("DateTime".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::UpdatedAt],
                },
            ]
        });

        ast.models.insert("Counter".to_string(), ModelNode { extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Counter".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("Int".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::AutoIncrement)],
                },
            ]
        });
        
        let tables = lower_ast_to_physical(&ast);
        assert_eq!(tables.len(), 2);
        
        let device = tables.iter().find(|t| t.name == "Device").unwrap();
        let id_col = device.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id_col.sqlite_type, "TEXT PRIMARY KEY DEFAULT (gen_uuid7())");
        
        assert_eq!(device.indexes.len(), 1);
        assert_eq!(device.indexes[0].name, "idx_Device_serial");
        assert!(device.indexes[0].unique);
        
        assert_eq!(device.triggers.len(), 1);
        assert!(device.triggers[0].name.contains("trg_update_Device_updated_at"));
        assert!(device.triggers[0].sql.contains("CREATE TRIGGER"));

        let counter = tables.iter().find(|t| t.name == "Counter").unwrap();
        let c_id_col = counter.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(c_id_col.sqlite_type, "INTEGER PRIMARY KEY AUTOINCREMENT");
    }

    #[test]
    fn test_lower_ast_to_physical_relations() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Post".to_string(), ModelNode { extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Post".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "authorId".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "author".to_string(),
                    field_type: AstFieldType::Relation("User".to_string()),
                    is_optional: false,
                    attributes: vec![
                        FieldAttribute::Relation {
                            name: None,
                            fields: vec!["authorId".to_string()],
                            references: vec!["id".to_string()],
                            on_delete: Some("Cascade".to_string()),
                            deferrable: false,
                            column: None,
                        }
                    ],
                },
            ]
        });
        
        let tables = lower_ast_to_physical(&ast);
        assert_eq!(tables.len(), 1);
        
        let post_table = tables.iter().find(|t| t.name == "Post").unwrap();
        assert_eq!(post_table.foreign_keys.len(), 1);
        assert_eq!(post_table.foreign_keys[0], "FOREIGN KEY (authorId) REFERENCES \"User\" (id) ON DELETE CASCADE");
    }

    #[test]
    fn test_ddl_ignores_abstract_bases() {
        let mut ast = SchemaAst {
            bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.bases.insert("Identifiable".to_string(), BaseNode {
            name: "Identifiable".to_string(),
            ..Default::default()
        });

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            ..Default::default()
        });

        let tables = lower_ast_to_physical(&ast);
        
        // Assert the base table does NOT exist
        assert!(tables.iter().find(|t| t.name == "Identifiable").is_none());
        
        // Assert the concrete model DOES exist
        let user_table = tables.iter().find(|t| t.name == "User").unwrap();
        assert_eq!(user_table.name, "User");
    }

    #[test]
    fn test_ddl_adds_synthetic_marker_columns_with_defaults() {
        let mut ast = SchemaAst {
            bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.models.insert("Manager".to_string(), ModelNode {
            name: "Manager".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("Int".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "__Employee".to_string(),
                    field_type: AstFieldType::Scalar("Boolean".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Default(DefaultFunc::Static("true".to_string()))],
                }
            ],
            ..Default::default()
        });

        let tables = lower_ast_to_physical(&ast);
        let user_table = tables.iter().find(|t| t.name == "Manager").unwrap();
        
        // Assert Physical Marker is mapped with DEFAULT 1 NOT NULL
        let marker = user_table.columns.iter().find(|c| c.name == "__Employee").unwrap();
        assert_eq!(marker.sqlite_type, "INTEGER DEFAULT 1 NOT NULL");
    }

    #[test]
    fn test_polymorphic_relations_drop_fks() {
        let mut ast = SchemaAst {
            bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.bases.insert("Vehicle".to_string(), BaseNode {
            name: "Vehicle".to_string(),
            ..Default::default()
        });

        ast.models.insert("Person".to_string(), ModelNode {
            name: "Person".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "carId".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                },
                FieldNode {
                    name: "car".to_string(),
                    field_type: AstFieldType::Relation("Vehicle".to_string()),
                    is_optional: false,
                    attributes: vec![
                        FieldAttribute::Relation {
                            name: None,
                            fields: vec!["carId".to_string()],
                            references: vec!["id".to_string()],
                            on_delete: None,
                            deferrable: false,
                            column: None,
                        }
                    ],
                }
            ],
            ..Default::default()
        });

        let tables = lower_ast_to_physical(&ast);
        let person_table = tables.iter().find(|t| t.name == "Person").unwrap();
        
        // The column should exist to store the ID
        assert!(person_table.columns.iter().any(|c| c.name == "carId"));
        
        // BUT the DB constraint MUST be intentionally omitted
        assert!(person_table.foreign_keys.is_empty());
    }
}
