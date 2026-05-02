use std::process::Command;
use std::env;
use std::fs;
use tempfile::tempdir;

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command failed: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", 
               cmd, 
               stdout,
               stderr);
    }
    stdout
}

#[tokio::test]
async fn test_e2e_destructive_migrations() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // 2. Define schema with a String column
    let schema_v1 = "
        model Config {

            value: String
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema_v1).unwrap();

    // 3. Migrate and Push
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Insert incompatible data
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Config (__id, value) VALUES ('c1', 'hello_world')", []).unwrap();
        db.execute("INSERT INTO Config (__id, value) VALUES ('c2', '42')", []).unwrap();
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();

    // 5. Evolve schema: Change `value` from String to Int
    let schema_v2 = "
        model Config {

            value: Int
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema_v2).unwrap();

    // 6. Run migrate. This will detect a type change and trigger a table rebuild.
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    println!("MIGRATE-DEV STDOUT:\n{}", stdout);
    println!("MIGRATE-DEV STDERR:\n{}", stderr);
    
    // SQLite uses dynamic typing ("manifest typing"). It will typically allow the rebuild,
    // storing 'hello_world' as TEXT even though the column affinity is INTEGER.
    // However, if we eventually enforce STRICT tables, this would throw a rollback.
    // For now, we assert that the migration completed and the data is preserved safely.
    assert!(output.status.success(), "Migration should succeed despite coercion");

    let conn2 = pool.get().await.unwrap();
    conn2.interact(|db| {
        let mut stmt = db.prepare("SELECT value FROM Config ORDER BY __id").unwrap();
        let rows: Vec<String> = stmt.query_map([], |row| {
            let val: rusqlite::types::Value = row.get(0)?;
            match val {
                rusqlite::types::Value::Text(s) => Ok(s),
                rusqlite::types::Value::Integer(i) => Ok(i.to_string()),
                rusqlite::types::Value::Real(f) => Ok(f.to_string()),
                rusqlite::types::Value::Null => Ok("NULL".to_string()),
                rusqlite::types::Value::Blob(_) => Ok("BLOB".to_string()),
            }
        }).unwrap().map(|r| r.unwrap()).collect();
        
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "hello_world");
        assert_eq!(rows[1], "42");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}