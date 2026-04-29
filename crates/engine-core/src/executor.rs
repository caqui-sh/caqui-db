use deadpool_sqlite::Pool;
use serde_json::Value;

pub async fn execute_compiled_read(pool: &Pool, sql: String) -> Result<Value, String> {
    let conn = pool.get().await.map_err(|e| e.to_string())?;
    
    // Perform blocking database I/O on a dedicated background thread (via interact) 
    // to prevent starving the Tokio async runtime
    let json_payload = conn.interact(move |db| -> Result<String, rusqlite::Error> {
        let mut stmt = db.prepare_cached(&sql)?;
        
        // SQLite returns exactly ONE string containing the entire requested graph
        let raw_json: String = stmt.query_row([], |row| row.get(0))?;
        Ok(raw_json)
    }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;

    // Zero-overhead deserialization directly into the dynamic API transport format
    let parsed_data: Value = serde_json::from_str(&json_payload).map_err(|e| e.to_string())?;

    Ok(parsed_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_compiled_read() {
        let pool = crate::pool::create_pool("file::memory:?cache=shared");

        let conn = pool.get().await.unwrap();
        conn.interact(|db| -> Result<(), rusqlite::Error> {
            db.execute(
                "CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);",
                [],
            )?;
            db.execute(
                "CREATE TABLE Post (id TEXT PRIMARY KEY, author_id TEXT, title TEXT);",
                [],
            )?;
            db.execute("INSERT INTO User (id, name) VALUES ('u1', 'Alice');", [])?;
            db.execute("INSERT INTO Post (id, author_id, title) VALUES ('p1', 'u1', 'Hello World');", [])?;
            db.execute("INSERT INTO Post (id, author_id, title) VALUES ('p2', 'u1', 'Another Post');", [])?;
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();

        // Simulate a query_compiler output using json_group_array
        let compiled_sql = "
            SELECT json_group_array(
                json_object(
                    'id', t0.id, 
                    'name', t0.name, 
                    'posts', (
                        SELECT json_group_array(json_object('id', t1.id, 'title', t1.title)) 
                        FROM Post AS t1 WHERE t1.author_id = t0.id
                    )
                )
            ) AS payload 
            FROM User AS t0;
        ".to_string();

        let json_value = execute_compiled_read(&pool, compiled_sql).await.unwrap();
        
        let users = json_value.as_array().unwrap();
        assert_eq!(users.len(), 1);
        
        let alice = users[0].as_object().unwrap();
        assert_eq!(alice["id"], "u1");
        assert_eq!(alice["name"], "Alice");
        
        let posts = alice["posts"].as_array().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0]["title"], "Hello World");
        assert_eq!(posts[1]["title"], "Another Post");
    }

    #[tokio::test]
    async fn test_execute_compiled_read_invalid_sql() {
        let pool = crate::pool::create_pool("file::memory:?cache=shared");
        let result = execute_compiled_read(&pool, "SELECT syntax error".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("syntax error"));
    }

    #[tokio::test]
    async fn test_execute_compiled_read_empty_result() {
        let pool = crate::pool::create_pool("file::memory:?cache=shared");
        // rusqlite's query_row returns Err(QueryReturnedNoRows) if zero rows found
        let result = execute_compiled_read(&pool, "SELECT 1 WHERE 1=0".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Query returned no rows"));
    }

    #[tokio::test]
    async fn test_execute_compiled_read_invalid_json() {
        let pool = crate::pool::create_pool("file::memory:?cache=shared");
        let conn = pool.get().await.unwrap();
        conn.interact(|db| -> Result<(), rusqlite::Error> {
            db.execute("CREATE TABLE Test (val TEXT);", [])?;
            db.execute("INSERT INTO Test (val) VALUES ('not a json');", [])?;
            Ok(())
        }).await.unwrap().unwrap();

        let result = execute_compiled_read(&pool, "SELECT val FROM Test".to_string()).await;
        assert!(result.is_err(), "Expected error for invalid JSON, got {:?}", result);
        // serde_json error for "not a json" usually contains "expected value", "invalid character", or "expected ident"
        let err = result.unwrap_err();
        assert!(
            err.contains("expected value") || err.contains("invalid character") || err.contains("expected ident"), 
            "Actual error: {}", err
        );
    }
}
