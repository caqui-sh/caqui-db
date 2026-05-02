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
                            ops.push(MigrationOp::AddColumn { 
                                table: des_table.name.clone(), 
                                column: des_col.clone() 
                            });
                        },
                        Some(live_col) => {
                            shared_cols.push(des_col.name.clone());
                            let type_changed = !live_col.sqlite_type.eq_ignore_ascii_case(&des_col.sqlite_type);
                            if type_changed {
                                requires_rebuild = true; 
                            }
                        }
                    }
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
                        // SQLite triggers are dropped if the table is dropped, 
                        // but here we just check if it exists.
                        // For simplicity, we always re-push triggers if not using RebuildTable
                        // actually we should check if trigger exists in live. 
                        // But introspection doesn't fetch triggers yet.
                        // So we always push CREATE TRIGGER IF NOT EXISTS.
                        ops.push(MigrationOp::CreateTrigger {
                            trigger: trigger.clone(),
                        });
                    }
                }
            }
        }
    }
    
    // Pass 2: Deletions (Drop Tables)
    for live_table_name in live.keys() {
        if !desired.iter().any(|dt| &dt.name == live_table_name) {
            ops.push(MigrationOp::DropTable { name: live_table_name.clone() });
        }
    }
    
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspection::LiveColumn;
    use crate::PhysicalIndex;

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
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols, indexes: vec![] });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::AddColumn { table, column } if table == "User" && column.name == "name"));
    }
}
