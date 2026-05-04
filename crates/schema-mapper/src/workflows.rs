use rusqlite::Connection;
use crate::PhysicalTable;
use crate::introspection::{fetch_live_tables, introspect_table_columns, LiveTable};
use crate::differ::compute_diff;
use crate::migration::generate_sql;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub fn get_live_schema(conn: &Connection) -> rusqlite::Result<HashMap<String, LiveTable>> {
    let mut live_schema = HashMap::new();
    let tables = fetch_live_tables(conn)?;
    for table_name in tables {
        let columns = introspect_table_columns(conn, &table_name)?;
        let indexes = crate::introspection::introspect_table_indexes(conn, &table_name)?;
        let foreign_keys = crate::introspection::introspect_table_foreign_keys(conn, &table_name)?;
        let triggers = crate::introspection::introspect_table_triggers(conn, &table_name)?;
        live_schema.insert(table_name.clone(), LiveTable {
            name: table_name,
            columns,
            indexes,
            foreign_keys,
            triggers,
        });
    }
    Ok(live_schema)
}

pub fn db_push(conn: &Connection, desired: &[PhysicalTable]) -> rusqlite::Result<()> {
    let live_schema = get_live_schema(conn)?;
    let ops = compute_diff(desired, &live_schema);
    
    for op in ops {
        let sql = generate_sql(&op);
        conn.execute_batch(&sql)?;
    }
    
    Ok(())
}

pub fn migrate_dev(desired: &[PhysicalTable], db_path: &str, migrations_dir: &str) -> rusqlite::Result<Option<String>> {
    let shadow_conn = Connection::open_in_memory()?;
    let migrations_path = Path::new(migrations_dir);
    if migrations_path.exists() && migrations_path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(migrations_path)
            .expect("Failed to read migrations directory")
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.path().extension().is_some_and(|ext| ext == "sql") {
                let sql = fs::read_to_string(entry.path()).expect("Failed to read migration file");
                shadow_conn.execute_batch(&sql)?;
            }
        }
    }
    let historical_schema = get_live_schema(&shadow_conn)?;
    let ops = compute_diff(desired, &historical_schema);
    if ops.is_empty() {
        return Ok(None);
    }
    let mut migration_sql = String::new();
    for op in ops {
        migration_sql.push_str(&generate_sql(&op));
    }
    if !migrations_path.exists() {
        fs::create_dir_all(migrations_path).expect("Failed to create migrations directory");
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let migration_filename = format!("{}_auto_migration.sql", timestamp);
    let migration_file_path = migrations_path.join(migration_filename);
    fs::write(migration_file_path, &migration_sql).expect("Failed to write migration file");
    let live_conn = Connection::open(db_path)?;
    live_conn.execute_batch(&migration_sql)?;
    live_conn.execute(
        "CREATE TABLE IF NOT EXISTS _engine_migrations (__id INTEGER PRIMARY KEY AUTOINCREMENT, applied_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
        [],
    )?;
    live_conn.execute("INSERT INTO _engine_migrations DEFAULT VALUES", [])?;
    Ok(Some(migration_sql))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PhysicalColumn;
    use tempfile::tempdir;

    #[test]
    fn test_db_push_workflow() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
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
        db_push(&conn, &desired).unwrap();
        let live = get_live_schema(&conn).unwrap();
        assert!(live.contains_key("User"));
    }

    #[test]
    fn test_migrate_dev_workflow() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let migrations_dir = dir.path().join("migrations");
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
        migrate_dev(&desired, db_path.to_str().unwrap(), migrations_dir.to_str().unwrap()).unwrap();
        assert!(migrations_dir.exists());
        let entries: Vec<_> = fs::read_dir(migrations_dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_migrate_dev_no_changes() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let migrations_dir = dir.path().join("migrations");
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
        migrate_dev(&desired, db_path.to_str().unwrap(), migrations_dir.to_str().unwrap()).unwrap();
        let result = migrate_dev(&desired, db_path.to_str().unwrap(), migrations_dir.to_str().unwrap()).unwrap();
        assert!(result.is_none());
    }
}
