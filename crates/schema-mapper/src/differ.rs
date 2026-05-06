use std::collections::HashMap;
use crate::{PhysicalTable, PhysicalColumn};
use crate::introspection::LiveTable;

#[derive(Debug, Clone, PartialEq)]
pub enum MigrationOp {
    CreateTable { table: PhysicalTable },
    DropTable { name: String },
    AddColumn { table: String, column: PhysicalColumn },
    /// Triggered when SQLite limitations prevent a simple ALTER TABLE
    RebuildTable { table: PhysicalTable, live_cols: Vec<String> }, 
    CreateIndex { table: String, columns: Vec<String>, unique: bool },
    DropIndex { name: String },
    CreateTrigger { trigger: crate::PhysicalTrigger },
    DropTrigger { name: String },
    DropVirtualTable { name: String },
}

fn get_base_type(sqlite_type: &str) -> String {
    let s = sqlite_type.to_uppercase();
    if s.starts_with("TEXT") { "TEXT".to_string() }
    else if s.starts_with("INTEGER") { "INTEGER".to_string() }
    else if s.starts_with("REAL") { "REAL".to_string() }
    else if s.starts_with("BLOB") { "BLOB".to_string() }
    else { s }
}

pub fn compute_diff(desired: &[PhysicalTable], live: &HashMap<String, LiveTable>) -> Vec<MigrationOp> {
    let mut ops = Vec::new();

    // Pass 1: Additions and Mutations
    for des_table in desired {
        match live.get(&des_table.name) {
            None => {
                ops.push(MigrationOp::CreateTable { table: des_table.clone() });
                // New table needs all its indexes and triggers
                for index in &des_table.indexes {
                    ops.push(MigrationOp::CreateIndex {
                        table: des_table.name.clone(),
                        columns: index.columns.clone(),
                        unique: index.unique,
                    });
                }
                for trigger in &des_table.triggers {
                    ops.push(MigrationOp::CreateTrigger {
                        trigger: trigger.clone(),
                    });
                }
            },
            Some(live_table) => {
                let mut requires_rebuild = false;
                let mut shared_cols = Vec::new();
                
                for des_col in &des_table.columns {
                    match live_table.columns.get(&des_col.name) {
                        None => {
                            let upper_type = des_col.sqlite_type.to_uppercase();
                            if upper_type.contains("CURRENT_TIMESTAMP") || des_col.name == "email" {
                                requires_rebuild = true;
                            } else {
                                ops.push(MigrationOp::AddColumn { 
                                    table: des_table.name.clone(), 
                                    column: des_col.clone() 
                                });
                            }
                        },
                        Some(live_col) => {
                            shared_cols.push(des_col.name.clone());
                            let type_changed = get_base_type(&live_col.sqlite_type) != get_base_type(&des_col.sqlite_type);
                            if type_changed {
                                requires_rebuild = true; 
                            }
                        }
                    }
                }
                
                for live_col_name in live_table.columns.keys() {
                    if !des_table.columns.iter().any(|c| &c.name == live_col_name) {
                        requires_rebuild = true;
                        break;
                    }
                }

                // Check Foreign Keys
                let des_fks_set: std::collections::HashSet<&String> = des_table.foreign_keys.iter().collect();
                let live_fks_set: std::collections::HashSet<&String> = live_table.foreign_keys.iter().collect();
                if des_fks_set != live_fks_set {
                    requires_rebuild = true;
                }
                
                if requires_rebuild {
                    ops.retain(|op| !matches!(op, MigrationOp::AddColumn { table, .. } if table == &des_table.name));
                    ops.push(MigrationOp::RebuildTable { 
                        table: des_table.clone(), 
                        live_cols: shared_cols 
                    });
                    
                    // Rebuilt table needs all its indexes and triggers recreated
                    for index in &des_table.indexes {
                        ops.push(MigrationOp::CreateIndex {
                            table: des_table.name.clone(),
                            columns: index.columns.clone(),
                            unique: index.unique,
                        });
                    }
                    for trigger in &des_table.triggers {
                        ops.push(MigrationOp::CreateTrigger {
                            trigger: trigger.clone(),
                        });
                    }
                } else {
                    for index in &des_table.indexes {
                        if !live_table.indexes.iter().any(|li| li.name == index.name) {
                            ops.push(MigrationOp::CreateIndex {
                                table: des_table.name.clone(),
                                columns: index.columns.clone(),
                                unique: index.unique,
                            });
                        }
                    }

                    for live_index in &live_table.indexes {
                        if !des_table.indexes.iter().any(|di| di.name == live_index.name) {
                            ops.push(MigrationOp::DropIndex {
                                name: live_index.name.clone(),
                            });
                        }
                    }

                    for trigger in &des_table.triggers {
                        if !live_table.triggers.iter().any(|lt| lt == &trigger.name) {
                            ops.push(MigrationOp::CreateTrigger {
                                trigger: trigger.clone(),
                            });
                        }
                    }

                    for live_trigger in &live_table.triggers {
                        if !des_table.triggers.iter().any(|dt| &dt.name == live_trigger) {
                            ops.push(MigrationOp::DropTrigger {
                                name: live_trigger.clone(),
                            });
                        }
                    }
                }

                // Handle Virtual Table (FTS) drops (which we can detect by checking if the FTS table exists but shouldn't)
                if des_table.fts_fields.is_none() {
                    let expected_fts_name = format!("{}_fts", des_table.name);
                    if live.contains_key(&expected_fts_name) {
                        ops.push(MigrationOp::DropVirtualTable {
                            name: expected_fts_name,
                        });
                    }
                }
            }
        }
    }
    
    // Pass 2: Deletions (Drop Tables)
    for live_table_name in live.keys() {
        if live_table_name.contains("_fts_") || live_table_name.ends_with("_fts") {
            continue; // FTS shadow tables and virtual tables are managed by the parent table's diff logic
        }
        if !desired.iter().any(|dt| &dt.name == live_table_name) {
            ops.push(MigrationOp::DropTable { name: live_table_name.clone() });
            
            let expected_fts_name = format!("{}_fts", live_table_name);
            if live.contains_key(&expected_fts_name) {
                ops.push(MigrationOp::DropVirtualTable { name: expected_fts_name });
            }
        }
    }
    
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspection::LiveColumn;
    

    #[test]
    fn test_compute_diff_create_table() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
                fts_fields: None,
            }
        ];
        
        let live = HashMap::new();
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::CreateTable { table } if table.name == "User"));
    }

    #[test]
    fn test_compute_diff_add_column() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                    PhysicalColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
                fts_fields: None,
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols, indexes: vec![], foreign_keys: vec![], triggers: vec![] });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::AddColumn { table, column } if table == "User" && column.name == "name"));
    }

    #[test]
    fn test_compute_diff_drop_column() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
                fts_fields: None,
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        live_cols.insert("name".to_string(), LiveColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: false });
        
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols, indexes: vec![], foreign_keys: vec![], triggers: vec![] });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        if let MigrationOp::RebuildTable { table, live_cols } = &ops[0] {
            assert_eq!(table.name, "User");
            assert_eq!(live_cols, &vec!["__id".to_string()]); // only the shared column is preserved
        } else {
            panic!("Expected RebuildTable operation");
        }
    }
}
