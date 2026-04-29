use deadpool_sqlite::{Manager, Pool, Runtime};
use rusqlite::functions::FunctionFlags;

pub fn register_custom_functions(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "gen_uuid7",
        0,
        FunctionFlags::SQLITE_UTF8,
        |_ctx| Ok(uuid::Uuid::now_v7().to_string()),
    )?;
    Ok(())
}

pub fn create_pool(path: &str) -> Pool {
    let cfg = deadpool_sqlite::Config::new(path);
    let manager = Manager::from_config(&cfg, Runtime::Tokio1);
    Pool::builder(manager)
        .post_create(deadpool::managed::Hook::async_fn(|conn: &mut <deadpool_sqlite::Manager as deadpool::managed::Manager>::Type, _| {
            Box::pin(async move {
                conn.interact(|db: &mut rusqlite::Connection| register_custom_functions(db))
                    .await
                    .map_err(|e| deadpool::managed::HookError::Message(e.to_string().into()))?
                    .map_err(|e| deadpool::managed::HookError::Message(e.to_string().into()))?;
                Ok(())
            })
        }))
        .build()
        .unwrap()
        }

        #[cfg(test)]
        mod tests {
        use super::*;

        #[tokio::test]
        async fn test_register_custom_functions_uuid7() {
        // Use an in-memory database for testing
        let pool = create_pool("file::memory:?cache=shared");
        let conn = pool.get().await.unwrap();

        let uuid: String = conn.interact(|db| {
            db.query_row("SELECT gen_uuid7()", [], |row| row.get(0))
        }).await.unwrap().unwrap();

        // Basic validation that it looks like a UUID
        assert_eq!(uuid.len(), 36);
        assert!(uuid.contains('-'));

        // Verify version 7 (the 13th character should be '7')
        // UUID format: xxxxxxxx-xxxx-Mxxx-Nxxx-xxxxxxxxxxxx
        // Version M is at index 14 (8+1+4+1)
        assert_eq!(uuid.chars().nth(14).unwrap(), '7');
        }
        }