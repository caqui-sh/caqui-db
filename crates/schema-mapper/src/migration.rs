use crate::differ::MigrationOp;
use crate::PhysicalTable;

pub fn generate_sql(op: &MigrationOp) -> String {
    match op {
        MigrationOp::CreateTable { table } => {
            let mut sql = generate_create_table_sql(&table.name, table) + ";\n";
            if let Some(ref fields) = table.fts_fields {
                sql.push_str(&format!("CREATE VIRTUAL TABLE IF NOT EXISTS {}_fts USING fts5({}, content='{}', content_rowid='rowid');\n", table.name, fields.join(", "), table.name));
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
            let temp_name = format!("_engine_new_{}", table.name);
            let create_temp = generate_create_table_sql(&temp_name, table);
            let cols_csv = live_cols.join(", ");
            
            let mut sql = format!(
                "PRAGMA foreign_keys=OFF;\n\
                 BEGIN TRANSACTION;\n\
                 {};\n\
                 INSERT INTO {} ({}) SELECT {} FROM {};\n\
                 DROP TABLE {};\n\
                 ALTER TABLE {} RENAME TO {};\n",
                create_temp, temp_name, cols_csv, cols_csv, table.name, table.name, temp_name, table.name
            );

            if let Some(ref fields) = table.fts_fields {
                sql.push_str(&format!("DROP TABLE IF EXISTS {}_fts;\n", table.name));
                sql.push_str(&format!("CREATE VIRTUAL TABLE IF NOT EXISTS {}_fts USING fts5(__id UNINDEXED, {}, content='{}');\n", table.name, fields.join(", "), table.name));
                sql.push_str(&format!("INSERT INTO {}_fts({}_fts) VALUES('rebuild');\n", table.name, table.name));
            }

            sql.push_str(
                "PRAGMA foreign_key_check;\n\
                 COMMIT;\n\
                 PRAGMA foreign_keys=ON;\n"
            );
            
            sql
        },
        MigrationOp::CreateIndex { table, columns, unique } => {
            let unique_str = if *unique { "UNIQUE " } else { "" };
            let cols_csv = columns.join("_");
            let index_name = format!("idx_{}_{}", table, cols_csv);
            format!("CREATE {}INDEX {} ON {} ({});\n", unique_str, index_name, table, columns.join(", "))
        },
        MigrationOp::DropIndex { name } => {
            format!("DROP INDEX {};\n", name)
        },
        MigrationOp::CreateTrigger { trigger } => {
            format!("{}\n", trigger.sql)
        },
        MigrationOp::DropTrigger { name } => {
            format!("DROP TRIGGER IF EXISTS {};\n", name)
        },
        MigrationOp::DropVirtualTable { name } => {
            format!("DROP TABLE IF EXISTS {};\n", name)
        },
    }
}

fn generate_create_table_sql(name: &str, table: &PhysicalTable) -> String {
    let mut sql = format!("CREATE TABLE {} (\n", name);
    let mut entries = Vec::new();
    for col in &table.columns {
        entries.push(format!("    {} {}", col.name, col.sqlite_type));
    }
    for fk in &table.foreign_keys {
        entries.push(format!("    {}", fk));
    }
    sql.push_str(&entries.join(",\n"));
    sql.push_str("\n)");
    sql
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
                PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
            ],
            indexes: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            fts_fields: None,
        };
        let sql = generate_create_table_sql("User", &table);
        assert_eq!(sql, "CREATE TABLE User (\n    __id TEXT PRIMARY KEY,\n    name TEXT\n)");
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
                PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "age".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false } // type changed
            ],
            indexes: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            fts_fields: None,
        };
        let live_cols = vec!["__id".to_string(), "age".to_string()];
        let op = MigrationOp::RebuildTable { table, live_cols };
        let sql = generate_sql(&op);
        
        let expected_sql = "PRAGMA foreign_keys=OFF;\n\
                            BEGIN TRANSACTION;\n\
                            CREATE TABLE _engine_new_User (\n    \
                                __id TEXT PRIMARY KEY,\n    \
                                age TEXT\n\
                            );\n\
                            INSERT INTO _engine_new_User (__id, age) SELECT __id, age FROM User;\n\
                            DROP TABLE User;\n\
                            ALTER TABLE _engine_new_User RENAME TO User;\n\
                            PRAGMA foreign_key_check;\n\
                            COMMIT;\n\
                            PRAGMA foreign_keys=ON;\n";
        
        assert_eq!(sql, expected_sql);
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
            unique: true,
        };
        let sql = generate_sql(&op);
        assert_eq!(sql, "CREATE UNIQUE INDEX idx_User_email ON User (email);\n");
    }

    #[test]
    fn test_generate_sql_drop_index() {
        let op = MigrationOp::DropIndex {
            name: "idx_User_email".to_string(),
        };
        let sql = generate_sql(&op);
        assert_eq!(sql, "DROP INDEX idx_User_email;\n");
    }

    #[test]
    fn test_generate_create_table_sql_with_fts() {
        let table = PhysicalTable {
            name: "Document".to_string(),
            columns: vec![
                PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "title".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
            ],
            indexes: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            fts_fields: Some(vec!["title".to_string(), "body".to_string()]),
        };
        let op = MigrationOp::CreateTable { table };
        let sql = generate_sql(&op);
        println!("GENERATED SQL:\n{}", sql);
        assert!(sql.contains("CREATE TABLE Document"));
        assert!(sql.contains("CREATE VIRTUAL TABLE IF NOT EXISTS Document_fts USING fts5(title, body, content='Document', content_rowid='rowid');\n"));
    }

    #[test]
    fn test_generate_rebuild_table_sql_with_fts() {
        let table = PhysicalTable {
            name: "Document".to_string(),
            columns: vec![
                PhysicalColumn { name: "__id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                PhysicalColumn { name: "title".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false }
            ],
            indexes: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            fts_fields: Some(vec!["title".to_string(), "body".to_string()]),
        };
        let live_cols = vec!["__id".to_string(), "title".to_string()];
        let op = MigrationOp::RebuildTable { table, live_cols };
        let sql = generate_sql(&op);
        println!("REBUILD SQL:\n{}", sql);
        assert!(sql.contains("DROP TABLE IF EXISTS Document_fts;\n"));
        assert!(sql.contains("CREATE VIRTUAL TABLE IF NOT EXISTS Document_fts USING fts5(__id UNINDEXED, title, body, content='Document');\n"));
        assert!(sql.contains("INSERT INTO Document_fts(Document_fts) VALUES('rebuild');\n"));
    }
}
