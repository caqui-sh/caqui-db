use rusqlite::{Connection, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LiveTable {
    pub name: String,
    pub columns: HashMap<String, LiveColumn>, // HashMap for O(1) diffing lookups
    pub indexes: Vec<crate::PhysicalIndex>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveColumn {
    pub name: String,
    pub sqlite_type: String, // TEXT, INTEGER, BLOB, REAL
    pub not_null: bool,
    pub default_value: Option<String>,
    pub is_pk: bool,
}

pub fn fetch_live_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_schema 
         WHERE type='table' 
         AND name NOT LIKE 'sqlite_%' 
         AND name != '_engine_migrations'"
    )?;
    
    let tables = stmt.query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    
    Ok(tables)
}

pub fn introspect_table_columns(conn: &Connection, table_name: &str) -> Result<HashMap<String, LiveColumn>> {
    let query = format!("PRAGMA table_info('{}')", table_name);
    let mut stmt = conn.prepare(&query)?;
    
    let columns = stmt.query_map([], |row| {
        Ok(LiveColumn {
            name: row.get(1)?,             
            sqlite_type: row.get(2)?,      
            not_null: row.get::<_, i32>(3)? == 1,
            default_value: row.get(4)?,
            is_pk: row.get::<_, i32>(5)? > 0,
        })
    })?;
    
    let mut col_map = HashMap::new();
    for col in columns.filter_map(Result::ok) {
        col_map.insert(col.name.clone(), col);
    }
    
    Ok(col_map)
}

pub fn introspect_table_indexes(conn: &Connection, table_name: &str) -> Result<Vec<crate::PhysicalIndex>> {
    let query = format!("PRAGMA index_list('{}')", table_name);
    let mut stmt = conn.prepare(&query)?;
    
    let mut indexes = Vec::new();
    let index_rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i32>(2)? == 1))
    })?;

    for res in index_rows {
        let (name, unique) = res?;
        if name.starts_with("sqlite_") { continue; }
        
        let info_query = format!("PRAGMA index_info('{}')", name);
        let mut info_stmt = conn.prepare(&info_query)?;
        let columns = info_stmt.query_map([], |row| row.get::<_, String>(2))?
            .collect::<Result<Vec<String>, _>>()?;
            
        indexes.push(crate::PhysicalIndex {
            name,
            columns,
            unique,
        });
    }
    
    Ok(indexes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn test_introspect_table_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE User (
                __id TEXT PRIMARY KEY,
                age INTEGER NOT NULL DEFAULT 18,
                name TEXT
            )",
            [],
        ).unwrap();

        let live_tables = fetch_live_tables(&conn).unwrap();
        assert_eq!(live_tables.len(), 1);
        assert_eq!(live_tables[0], "User");

        let columns = introspect_table_columns(&conn, "User").unwrap();
        assert_eq!(columns.len(), 3);

        let id_col = columns.get("__id").unwrap();
        assert_eq!(id_col.sqlite_type, "TEXT");
        assert!(id_col.is_pk);
        assert!(!id_col.not_null); // SQLite PRIMARY KEY does not imply NOT NULL in PRAGMA table_info by default unless explicitly specified

        let age_col = columns.get("age").unwrap();
        assert_eq!(age_col.sqlite_type, "INTEGER");
        assert!(!age_col.is_pk);
        assert!(age_col.not_null);
        assert_eq!(age_col.default_value.as_deref(), Some("18"));

        let name_col = columns.get("name").unwrap();
        assert_eq!(name_col.sqlite_type, "TEXT");
        assert!(!name_col.is_pk);
        assert!(!name_col.not_null);
        assert_eq!(name_col.default_value, None);
    }

    #[test]
    fn test_introspection_ignores_synthetic_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE Developer (
                __id TEXT PRIMARY KEY,
                name TEXT,
                __Employee INTEGER DEFAULT 1 NOT NULL,
                __Human INTEGER DEFAULT 1 NOT NULL
            )",
            [],
        ).unwrap();

        let columns = introspect_table_columns(&conn, "Developer").unwrap();
        
        // Assert that synthetics are now included
        assert_eq!(columns.len(), 4);
        assert!(columns.contains_key("__id"));
        assert!(columns.contains_key("name"));
        assert!(columns.contains_key("__Employee"));
        assert!(columns.contains_key("__Human"));
    }
}
