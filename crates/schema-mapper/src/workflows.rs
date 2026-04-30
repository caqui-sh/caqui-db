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
        live_schema.insert(table_name.clone(), LiveTable {
            name: table_name,
            columns,
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
    // 1. Boot the Shadow DB
    let shadow_conn = Connection::open_in_memory()?;
    
    // 2. Replay History
    let migrations_path = Path::new(migrations_dir);
    if migrations_path.exists() && migrations_path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(migrations_path)
            .expect("Failed to read migrations directory")
            .filter_map(Result::ok)
            .collect();
            
        // Sort chronologically
        entries.sort_by_key(|entry| entry.file_name());
        
        for entry in entries {
            if entry.path().extension().is_some_and(|ext| ext == "sql") {
                let sql = fs::read_to_string(entry.path()).expect("Failed to read migration file");
                shadow_conn.execute_batch(&sql)?;
            }
        }
    }
    
    // 3. Shadow Introspection
    let historical_schema = get_live_schema(&shadow_conn)?;
    
    // 4. Compute Delta
    let ops = compute_diff(desired, &historical_schema);
    
    if ops.is_empty() {
        return Ok(None);
    }
    
    // 5. Write Migration File
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
    
    // 6. Apply to Live
    let live_conn = Connection::open(db_path)?;
    live_conn.execute_batch(&migration_sql)?;
    
    // Insert timestamp into tracking table
    live_conn.execute(
        "CREATE TABLE IF NOT EXISTS _engine_migrations (id INTEGER PRIMARY KEY AUTOINCREMENT, applied_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
        [],
    )?;
    live_conn.execute("INSERT INTO _engine_migrations DEFAULT VALUES", [])?;
    
    Ok(Some(migration_sql))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhysicalTable, PhysicalColumn};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_migrate_dev_workflow() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("app.db");
        let migrations_dir = dir.path().join("migrations");
        
        // Create an initial legacy migration
        fs::create_dir_all(&migrations_dir).unwrap();
        let old_migration = migrations_dir.join("001_init.sql");
        fs::write(
            &old_migration, 
            "CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);"
        ).unwrap();

        // The developer's new desired AST has an added 'age' column
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                    PhysicalColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                    PhysicalColumn { name: "age".to_string(), sqlite_type: "INTEGER".to_string(), is_json_array: false },
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];

        // Initialize live database with old schema
        let live_conn = Connection::open(&db_path).unwrap();
        live_conn.execute("CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);", []).unwrap();

        // Run the workflow
        let generated_sql = migrate_dev(&desired, db_path.to_str().unwrap(), migrations_dir.to_str().unwrap()).unwrap();
        
        assert!(generated_sql.is_some());
        let sql = generated_sql.unwrap();
        assert!(sql.contains("ALTER TABLE User ADD COLUMN age INTEGER;"));
        
        // Assert a new migration file was written
        let files: Vec<_> = fs::read_dir(&migrations_dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(files.len(), 2); // 001_init.sql + <timestamp>_auto_migration.sql
    }

    #[test]
    fn test_db_push_workflow() {
        let conn = Connection::open_in_memory().unwrap();
        
        // Initial state: One table
        let desired_1 = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];
        
        db_push(&conn, &desired_1).unwrap();
        
        // Verify table exists
        let table_names = fetch_live_tables(&conn).unwrap();
        assert!(table_names.contains(&"User".to_string()));
        
        // Update state: Add column and add new table
        let desired_2 = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                    PhysicalColumn { name: "age".to_string(), sqlite_type: "INTEGER".to_string(), is_json_array: false },
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            },
            PhysicalTable {
                name: "Post".to_string(),
                columns: vec![
                    PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT PRIMARY KEY".to_string(), is_json_array: false },
                    PhysicalColumn { name: "title".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];
        
        db_push(&conn, &desired_2).unwrap();
        
        // Verify User table was updated
        let user_cols = introspect_table_columns(&conn, "User").unwrap();
        assert!(user_cols.contains_key("age"));
        
        // Verify Post table was created
        let table_names_final = fetch_live_tables(&conn).unwrap();
        assert!(table_names_final.contains(&"Post".to_string()));
    }

    #[test]
    fn test_migrate_dev_no_changes() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("app.db");
        let migrations_dir = dir.path().join("migrations");
        
        // Create an initial legacy migration
        fs::create_dir_all(&migrations_dir).unwrap();
        let old_migration = migrations_dir.join("001_init.sql");
        fs::write(
            &old_migration, 
            "CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);"
        ).unwrap();

        // The developer's desired AST perfectly matches the historical state
        let desired = vec![
            PhysicalTable {
                name: "User".to_string(),
                columns: vec![
                    PhysicalColumn { name: "id".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                    PhysicalColumn { name: "name".to_string(), sqlite_type: "TEXT".to_string(), is_json_array: false },
                ],
                indexes: vec![],
                triggers: vec![],
                foreign_keys: vec![],
            }
        ];

        // Initialize live database with old schema
        let live_conn = Connection::open(&db_path).unwrap();
        live_conn.execute("CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);", []).unwrap();

        // Run the workflow
        let generated_sql = migrate_dev(&desired, db_path.to_str().unwrap(), migrations_dir.to_str().unwrap()).unwrap();
        
        // Ensure no SQL was generated (early exit path taken)
        assert!(generated_sql.is_none());
        
        // Ensure no new migration file was written
        let files: Vec<_> = fs::read_dir(&migrations_dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(files.len(), 1, "Expected only the 001_init.sql to exist");
    }
}
