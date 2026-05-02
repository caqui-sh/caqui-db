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
}

pub fn compute_diff(desired: &[PhysicalTable], live: &HashMap<String, LiveTable>) -> Vec<MigrationOp> {
    let mut ops = Vec::new();

    // Pass 1: Additions and Mutations
    for des_table in desired {
        match live.get(&des_table.name) {
            None => ops.push(MigrationOp::CreateTable { table: des_table.clone() }),
            Some(live_table) => {
                let mut requires_rebuild = false;
                let mut shared_cols = Vec::new();
                
                for des_col in &des_table.columns {
                    match live_table.columns.get(&des_col.name) {
                        None => {
                            // Column exists in desired but not in live -> AddColumn
                            // Note: if requires_rebuild is later flagged, this AddColumn is moot.
                            ops.push(MigrationOp::AddColumn { 
                                table: des_table.name.clone(), 
                                column: des_col.clone() 
                            });
                        },
                        Some(live_col) => {
                            shared_cols.push(des_col.name.clone());
                            
                            // SQLite types in PRAGMA table_info can sometimes vary in exact casing
                            // and precision based on how they were created, but we compare loosely here.
                            // We trigger a rebuild if the type doesn't match roughly, or if a NOT NULL constraint is added.
                            // (We don't support dropping columns without a rebuild either).
                            
                            // NOTE: A more robust check might be needed for sqlite_type matching.
                            let type_changed = !live_col.sqlite_type.eq_ignore_ascii_case(&des_col.sqlite_type);
                            
                            // Desired is not null, but live is nullable -> destructive change -> rebuild
                            // (Note: standard SQLite ALTER TABLE ADD COLUMN allows NOT NULL only if a DEFAULT is provided, 
                            // but modifying an existing column's nullability is not supported directly).
                            let not_null_changed = false; // We don't model NOT NULL yet in the parser/mapper, but stubbing it for future.
                            
                            if type_changed || not_null_changed {
                                requires_rebuild = true; 
                            }
                        }
                    }
                }
                
                // If a constraint change requires it, trigger the atomic rebuild
                if requires_rebuild {
                    // Prune previous AddColumn ops for this table as they are now moot
                    ops.retain(|op| !matches!(op, MigrationOp::AddColumn { table, .. } if table == &des_table.name));
                    ops.push(MigrationOp::RebuildTable { 
                        table: des_table.clone(), 
                        live_cols: shared_cols 
                    });
                } else {
                    for index in &des_table.indexes {
                        ops.push(MigrationOp::CreateIndex {
                            table: des_table.name.clone(),
                            columns: index.columns.clone(),
                            unique: index.unique,
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
        
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::AddColumn { table, column } if table == "User" && column.name == "name"));
    }

    #[test]
    fn test_compute_diff_rebuild_table() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "INTEGER".to_string(), is_json_array: false }
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        // Live is TEXT, Desired is INTEGER -> triggers rebuild
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::RebuildTable { table, .. } if table.name == "User"));
    }

    #[test]
    fn test_compute_diff_drop_table() {
        let desired = vec![];
        
        let mut live = HashMap::new();
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: HashMap::new() });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::DropTable { name } if name == "User"));
    }

    #[test]
    fn test_compute_diff_no_op() {
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
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols });
        
        let ops = compute_diff(&desired, &live);
        
        assert!(ops.is_empty(), "Differ should be idempotent and return empty ops for identical schemas");
    }

    #[test]
    fn test_compute_diff_create_index() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
                ],
                indexes: vec![
                    PhysicalIndex { name: "idx_User_email".to_string(), columns: vec!["email".to_string()], unique: true }
                ],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols });
        
        let ops = compute_diff(&desired, &live);
        
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::CreateIndex { table, columns, unique } if table == "User" && columns[0] == "email" && *unique));
    }

    #[test]
    fn test_compute_diff_emits_indexes_on_unchanged_table() {
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
                ],
                indexes: vec![
                    PhysicalIndex { name: "idx_User_id_polymorphic".to_string(), columns: vec!["__id".to_string()], unique: false }
                ],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];
        
        let mut live = HashMap::new();
        let mut live_cols = HashMap::new();
        live_cols.insert("__id".to_string(), LiveColumn { name: "__id".to_string(), sqlite_type: "TEXT".to_string(), not_null: false, default_value: None, is_pk: true });
        
        // The table columns perfectly match, so it won't be rebuilt
        live.insert("User".to_string(), LiveTable { name: "User".to_string(), columns: live_cols });
        
        let ops = compute_diff(&desired, &live);
        
        // Assert that the index creation op is still emitted even though the table is identical
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::CreateIndex { table, columns, unique } if table == "User" && columns[0] == "__id" && !*unique));
    }
}
