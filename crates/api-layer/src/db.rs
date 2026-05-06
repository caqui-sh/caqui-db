use deadpool_sqlite::{Manager, Pool, Runtime};
use rusqlite::functions::FunctionFlags;

pub fn register_custom_functions(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "gen_uuid7",
        0,
        FunctionFlags::SQLITE_UTF8,
        |_ctx| Ok(uuid::Uuid::now_v7().to_string()),
    )?;
    conn.create_scalar_function(
        "gen_cuid",
        0,
        FunctionFlags::SQLITE_UTF8,
        |_ctx| Ok(cuid::cuid1().unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())),
    )?;
    Ok(())
}

pub fn create_pool(path: &str) -> Pool {
    let cfg = deadpool_sqlite::Config::new(path);
    let manager = Manager::from_config(&cfg, Runtime::Tokio1);
    Pool::builder(manager)
        .post_create(deadpool::managed::Hook::async_fn(|conn: &mut <deadpool_sqlite::Manager as deadpool::managed::Manager>::Type, _| {
            Box::pin(async move {
                conn.interact(|db: &mut rusqlite::Connection| {
                    db.execute_batch("PRAGMA foreign_keys = ON;")?;
                    register_custom_functions(db)
                })
                    .await
                    .map_err(|e| deadpool::managed::HookError::Message(e.to_string().into()))?
                    .map_err(|e| deadpool::managed::HookError::Message(e.to_string().into()))?;
                Ok(())
            })
        }))
        .build()
        .unwrap()
}
