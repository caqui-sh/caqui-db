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

        // Pass 1: Columns, Triggers, and Indexes
        for field in &model.resolved_fields {
            let is_unique = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Unique));
            if is_unique {
                indexes.push(PhysicalIndex {
                    name: format!("idx_{}_{}", model.name, field.name),
                    columns: vec![field.name.clone()],
                    unique: true,
                });
            }

            let mut is_updated_at = false;
            let mut target_tracked_fields: Vec<String> = Vec::new();
            for attr in &field.attributes {
                match attr {
                    FieldAttribute::InternalTracked => is_updated_at = true,
                    FieldAttribute::InternalFieldTracked(target) => {
                        is_updated_at = true;
                        // Resolve physical columns for the target field
                        if let Some(target_field) = model.resolved_fields.iter().find(|f| &f.name == target) {
                            if let Some(FieldAttribute::InternalRelation { fields, .. }) = target_field.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                                if !fields.is_empty() {
                                    target_tracked_fields.extend(fields.clone());
                                } else {
                                    // Implicit relation foreign key
                                    target_tracked_fields.push(format!("{}Id", target));
                                }
                            } else if matches!(target_field.field_type, AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicBase(_)) {
                                target_tracked_fields.push(format!("{}_type", target));
                                target_tracked_fields.push(format!("{}_id", target));
                            } else {
                                target_tracked_fields.push(target.clone());
                            }
                        } else {
                            target_tracked_fields.push(target.clone());
                        }
                    }
                    _ => {}
                }
            }

            if is_updated_at {
                let trigger_name = format!("trg_update_{}_{}", model.name, field.name);
                let on_clause = if !target_tracked_fields.is_empty() {
                    format!("AFTER UPDATE OF {} ON {}", target_tracked_fields.join(", "), model.name)
                } else {
                    format!("AFTER UPDATE ON {}", model.name)
                };
                let sql = format!(
                    "CREATE TRIGGER IF NOT EXISTS {} \n\
                     {} \n\
                     FOR EACH ROW \n\
                     WHEN OLD.{} IS NULL OR NEW.{} <= OLD.{} \n\
                     BEGIN \n\
                         UPDATE {} SET {} = CURRENT_TIMESTAMP WHERE __id = OLD.__id; \n\
                     END;",
                    trigger_name, on_clause, field.name, field.name, field.name, model.name, field.name
                );
                triggers.push(PhysicalTrigger {
                    name: trigger_name,
                    sql,
                });
            }

            match &field.field_type {
                AstFieldType::ScalarArray(_) | AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_) => {
                    columns.push(PhysicalColumn {
                        name: field.name.clone(),
                        sqlite_type: "TEXT".to_string(), // Tagged internally for JSON1
                        is_json_array: true,             
                    });
                },
                AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicBase(_) => {
                    // Drop original field; inject discriminator string and ID pointer
                    let type_col = format!("{}_type", field.name);
                    let id_col = format!("{}_id", field.name);
                    
                    columns.push(PhysicalColumn {
                        name: type_col.clone(),
                        sqlite_type: "TEXT".to_string(),
                        is_json_array: false,
                    });
                    columns.push(PhysicalColumn {
                        name: id_col.clone(),
                        sqlite_type: "TEXT".to_string(), // Or INTEGER depending on PK definition, assuming TEXT
                        is_json_array: false,
                    });
                    
                    indexes.push(PhysicalIndex {
                        name: format!("idx_{}_{}_polymorphic", model.name, field.name),
                        columns: vec![type_col, id_col],
                        unique: false,
                    });
                },
                AstFieldType::Scalar(t) => {
                    let is_id = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id));
                    let is_autoincrement = field.attributes.iter().any(|a| matches!(a, FieldAttribute::InternalDefault(DefaultFunc::AutoIncrement)));
                    let is_uuid = field.attributes.iter().any(|a| matches!(a, FieldAttribute::InternalDefault(DefaultFunc::Uuid)));
                    let is_cuid = field.attributes.iter().any(|a| matches!(a, FieldAttribute::InternalDefault(DefaultFunc::Cuid)));
                    let is_updated_at = field.attributes.iter().any(|a| matches!(a, FieldAttribute::InternalTracked) || matches!(a, FieldAttribute::InternalFieldTracked(_)));

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
                        } else if is_cuid {
                            sql_type = format!("{} DEFAULT (gen_cuid())", sql_type);
                        }
                    } else if is_updated_at {
                        sql_type = format!("{} DEFAULT CURRENT_TIMESTAMP", sql_type);
                    } else if field.name.starts_with("__") {
                        if let Some(FieldAttribute::InternalDefault(DefaultFunc::Static(val))) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalDefault(DefaultFunc::Static(_)))) {
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
                _ => {}
            }
        }

        // Pass 2: Foreign Keys (now that all columns are present)
        for field in &model.resolved_fields {
            if let AstFieldType::Relation(ref_model) = &field.field_type {
                // Skip foreign key generation if the target is an abstract base
                let is_base_target = ast.bases.contains_key(ref_model);

                if !is_base_target {
                    let mut fk_fields = Vec::new();
                    let mut fk_references = Vec::new();
                    if let Some(FieldAttribute::InternalRelation { fields, references }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                        fk_fields = fields.clone();
                        fk_references = references.clone();
                    }
                    
                    if let Some(FieldAttribute::Relation { on_delete, .. }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if !fk_fields.is_empty() && !fk_references.is_empty() {
                            // CRITICAL: Only generate a physical FOREIGN KEY if the columns actually exist in this table.
                            let all_fields_exist = fk_fields.iter().all(|f| columns.iter().any(|c| &c.name == f));

                            if all_fields_exist {
                                let mut fk_def = format!("FOREIGN KEY ({}) REFERENCES \"{}\" ({})", fk_fields.join(", "), ref_model, fk_references.join(", "));
                                
                                if let Some(action) = on_delete {
                                    let sql_action = match action.to_uppercase().as_str() {
                                        "CASCADE" => "CASCADE",
                                        "SETNULL" | "SET NULL" => "SET NULL",
                                        "RESTRICT" => "RESTRICT",
                                        "SETDEFAULT" | "SET DEFAULT" => "SET DEFAULT",
                                        _ => "NO ACTION"
                                    };
                                    fk_def.push_str(&format!(" ON DELETE {}", sql_action));
                                }

                                fk_def.push_str(" DEFERRABLE INITIALLY DEFERRED");
                                foreign_keys.push(fk_def);
                            }
                        }
                    }
                }
            }
        }

        physical_tables.push(PhysicalTable {
            name: model.name.clone(),
            columns,
            indexes,
            triggers,
            foreign_keys,
        });
    }

    physical_tables
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lower_ast_to_physical() {
        let mut ast = SchemaAst::default();
        let mut model = ModelNode {
            name: "User".to_string(),
            ..Default::default()
        };
        model.resolved_fields.push(FieldNode {
            name: "__id".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::Id, FieldAttribute::InternalDefault(DefaultFunc::Uuid)],
        });
        ast.models.insert("User".to_string(), model);

        let physical = lower_ast_to_physical(&ast);
        assert_eq!(physical.len(), 1);
        assert_eq!(physical[0].name, "User");
        assert_eq!(physical[0].columns.len(), 1);
        assert_eq!(physical[0].columns[0].name, "__id");
        assert!(physical[0].columns[0].sqlite_type.contains("PRIMARY KEY"));
        assert!(physical[0].columns[0].sqlite_type.contains("gen_uuid7()"));
    }

    #[test]
    fn test_ddl_ignores_abstract_bases() {
        let mut ast = SchemaAst::default();
        ast.bases.insert("Identifiable".to_string(), BaseNode {
            name: "Identifiable".to_string(),
            extends: vec![],
            fields: vec![],
            resolved_fields: vec![],
            resolved_bases: std::collections::BTreeSet::new(),
        });
        
        let physical = lower_ast_to_physical(&ast);
        assert_eq!(physical.len(), 0, "Abstract bases should not produce physical tables");
    }

    #[test]
    fn test_ddl_adds_synthetic_marker_columns_with_defaults() {
        let mut ast = SchemaAst::default();
        let mut model = ModelNode {
            name: "Developer".to_string(),
            ..Default::default()
        };
        model.resolved_fields.push(FieldNode {
            name: "__id".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::Id],
        });
        model.resolved_fields.push(FieldNode {
            name: "__Employee".to_string(),
            field_type: AstFieldType::Scalar("Boolean".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::InternalDefault(DefaultFunc::Static("true".to_string()))],
        });
        model.resolved_fields.push(FieldNode {
            name: "__kind".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::InternalDefault(DefaultFunc::Static("Developer".to_string()))],
        });
        ast.models.insert("Developer".to_string(), model);

        let physical = lower_ast_to_physical(&ast);
        let table = &physical[0];
        
        let emp_col = table.columns.iter().find(|c| c.name == "__Employee").unwrap();
        assert_eq!(emp_col.sqlite_type, "INTEGER DEFAULT 1 NOT NULL");

        let kind_col = table.columns.iter().find(|c| c.name == "__kind").unwrap();
        assert_eq!(kind_col.sqlite_type, "TEXT DEFAULT 'Developer' NOT NULL");
    }
}
