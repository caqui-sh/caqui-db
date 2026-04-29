use crate::differ::MigrationOp;
use crate::PhysicalTable;

pub fn generate_create_table_sql(table_name: &str, table: &PhysicalTable) -> String {
    let mut sql = format!("CREATE TABLE {} (\n", table_name);
    let mut cols = Vec::new();
    for col in &table.columns {
        cols.push(format!("    {} {}", col.name, col.sqlite_type));
    }
    sql.push_str(&cols.join(",\n"));
    sql.push_str("\n)");
    sql
}

pub fn generate_sql(op: &MigrationOp) -> String {
    match op {
        MigrationOp::CreateTable { table } => {
            let mut sql = generate_create_table_sql(&table.name, table);
            sql.push_str(";\n");
            for index in &table.indexes {
                let unique_str = if index.unique { "UNIQUE " } else { "" };
                sql.push_str(&format!("CREATE {}INDEX {} ON {} ({});\n", unique_str, index.name, table.name, index.columns.join(", ")));
            }
            for trigger in &table.triggers {
                sql.push_str(&trigger.sql);
                sql.push('\n');
            }
            sql
        },
        MigrationOp::DropTable { name } => {
            format!("DROP TABLE {};\n", name)
        },
        MigrationOp::AddColumn { table, column } => {
            format!("ALTER TABLE {} ADD COLUMN {} {};\n", table, column.name, column.sqlite_type)
        },
        MigrationOp::RebuildTable { table, live_cols } => {
            let orig_name = &table.name;
            let temp_name = format!("_engine_new_{}", orig_name);
            let create_sql = generate_create_table_sql(&temp_name, table);
            let cols_csv = live_cols.join(", ");
            
            // The Atomic SQLite Table Rebuild Sequence
            let mut sql = format!(
                "PRAGMA foreign_keys=OFF;\n\
                 BEGIN TRANSACTION;\n\
                 {create_sql};\n\
                 INSERT INTO {temp} ({cols}) SELECT {cols} FROM {orig};\n\
                 DROP TABLE {orig};\n\
                 ALTER TABLE {temp} RENAME TO {orig};\n\
                 PRAGMA foreign_key_check;\n",
                temp = temp_name,
                orig = orig_name,
                cols = cols_csv
            );

            for index in &table.indexes {
                let unique_str = if index.unique { "UNIQUE " } else { "" };
                sql.push_str(&format!("CREATE {}INDEX {} ON {} ({});\n", unique_str, index.name, table.name, index.columns.join(", ")));
            }
            for trigger in &table.triggers {
                sql.push_str(&trigger.sql);
                sql.push('\n');
            }

            sql.push_str("COMMIT;\nPRAGMA foreign_keys=ON;\n");
            sql
        },
        MigrationOp::CreateIndex { table, columns, unique } => {
            let unique_str = if *unique { "UNIQUE " } else { "" };
            let cols_csv = columns.join("_");
            let index_name = format!("idx_{}_{}", table, cols_csv);
            format!("CREATE {}INDEX {} ON {} ({});\n", unique_str, index_name, table, columns.join(", "))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhysicalColumn, PhysicalIndex, PhysicalTrigger};

    #[test]
    fn test_generate_create_table_sql() {
        let table = PhysicalTable {
            name: "User".to_string(),
            columns: vec![
                PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
            ],
            indexes: vec![],
            triggers: vec![],
        };
        let sql = generate_create_table_sql("User", &table);
        assert_eq!(sql, "CREATE TABLE User (\n    id TEXT PRIMARY KEY,\n    name TEXT\n)");
    }

    #[test]
    fn test_generate_sql_add_column() {
        let op = MigrationOp::AddColumn {
            table: "User".to_string(),
            column: PhysicalColumn { name: "age".to_string(), sqlite_type: "INTEGER".to_string(), is_json_array: false }
        };
        let sql = generate_sql(&op);
        assert_eq!(sql, "ALTER TABLE User ADD COLUMN age INTEGER;\n");
    }

    #[test]
    fn test_generate_sql_rebuild_table() {
        let table = PhysicalTable {
            name: "User".to_string(),
            columns: vec![
                PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "age".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false } // type changed
            ],
            indexes: vec![],
            triggers: vec![],
        };
        let live_cols = vec!["id".to_string(), "age".to_string()];
        let op = MigrationOp::RebuildTable { table, live_cols };
        let sql = generate_sql(&op);
        
        assert!(sql.contains("PRAGMA foreign_keys=OFF;"));
        assert!(sql.contains("CREATE TABLE _engine_new_User"));
        assert!(sql.contains("INSERT INTO _engine_new_User (id, age) SELECT id, age FROM User;"));
        assert!(sql.contains("DROP TABLE User;"));
        assert!(sql.contains("ALTER TABLE _engine_new_User RENAME TO User;"));
    }

    #[test]
    fn test_generate_sql_drop_table() {
        let op = MigrationOp::DropTable { name: "OldTable".to_string() };
        let sql = generate_sql(&op);
        assert_eq!(sql, "DROP TABLE OldTable;\n");
    }

    #[test]
    fn test_generate_sql_create_index() {
        let op = MigrationOp::CreateIndex { 
            table: "User".to_string(), 
            columns: vec!["email".to_string()], 
            unique: true 
        };
        let sql = generate_sql(&op);
        assert_eq!(sql, "CREATE UNIQUE INDEX idx_User_email ON User (email);\n");
    }

    #[test]
    fn test_generate_create_table_with_auxiliary() {
        let table = PhysicalTable {
            name: "Device".to_string(),
            columns: vec![
                PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
            ],
            indexes: vec![
                PhysicalIndex { name: "idx_Device_id".to_string(), columns: vec!["id".to_string()], unique: true }
            ],
            triggers: vec![
                PhysicalTrigger { name: "trg_test".to_string(), sql: "CREATE TRIGGER trg_test AFTER INSERT ON Device BEGIN SELECT 1; END;".to_string() }
            ],
        };
        let op = MigrationOp::CreateTable { table };
        let sql = generate_sql(&op);
        
        assert!(sql.contains("CREATE TABLE Device"));
        assert!(sql.contains("CREATE UNIQUE INDEX idx_Device_id ON Device (id);"));
        assert!(sql.contains("CREATE TRIGGER trg_test"));
    }
}
