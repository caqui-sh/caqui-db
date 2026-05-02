use std::process::{Command};
use std::env;
use std::fs;
use tempfile::tempdir;

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command failed: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", cmd, stdout, stderr);
    }
    stdout
}

#[test]
fn test_e2e_ddl_ignores_bases_and_injects_markers() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    let schema = r#"
        base Identifiable {  }
        model User extends Identifiable { name: String @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);
    
    let db_path = workspace.join("app.db");
    assert!(db_path.exists());
    
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();
    
    // 1. MUST NOT emit a table for the base
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='Identifiable'").unwrap();
    let mut rows = stmt.query([]).unwrap();
    assert!(rows.next().unwrap().is_none(), "Table 'Identifiable' was created, but it shouldn't have been.");
    
    // 2. MUST emit the concrete table with inherited fields AND the synthetic marker
    let mut stmt = conn.prepare("PRAGMA table_info(User)").unwrap();
    let mut rows = stmt.query([]).unwrap();
    
    let mut columns = std::collections::HashSet::new();
    while let Some(row) = rows.next().unwrap() {
        let name: String = row.get(1).unwrap();
        columns.insert(name);
    }
    
    assert!(columns.contains("__id"), "Inherited 'id' column missing.");
    assert!(columns.contains("name"), "Native 'name' column missing.");
    assert!(columns.contains("__Identifiable"), "Synthetic '__Identifiable' marker missing.");
    assert!(columns.contains("__User"), "Synthetic '__User' model marker missing.");
    assert!(columns.contains("__kind"), "Synthetic '__kind' marker missing.");
}

#[test]
fn test_e2e_retroactive_trait_implementation() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    // Stage 1: Standalone model
    fs::write(workspace.join("schema.cq"), "model Post { text: String @@id(uuid) }").unwrap();
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();
    conn.execute("INSERT INTO Post (__id, text) VALUES ('1', 'Hello World')", []).unwrap();

    // Stage 2: Abstract trait introduced and inherited
    let v2_schema = r#"
        base Auditable { updatedAt: String }
        model Post extends Auditable { text: String @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), v2_schema).unwrap();
    let mut cmd2 = Command::new(caqui_bin);
    cmd2.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd2);

    // Reconnect to ensure we see the latest schema
    let conn2 = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();

    // Verify retroactive columns were added with defaults
    let mut stmt = conn2.prepare("SELECT __Auditable FROM Post WHERE __id = '1'").unwrap();
    let mut rows = stmt.query([]).unwrap();
    let row = rows.next().unwrap().expect("Row 1 should exist");
    
    let auditable_flag: i64 = row.get(0).unwrap();
    assert_eq!(auditable_flag, 1, "Retroactive trait implementation failed to set default 1.");
}

#[test]
fn test_e2e_track_migration() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    // Initial Schema without @@track
    let schema_v1 = r#"
        model Config {
            settings: String
            @@id(uuid)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema_v1).unwrap();

    let mut cmd1 = Command::new(caqui_bin);
    cmd1.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd1);

    // Now update schema to include @@track
    let schema_v2 = r#"
        model Config {
            settings: String
            @@track
            @@id(uuid)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema_v2).unwrap();

    let mut cmd2 = Command::new(caqui_bin);
    cmd2.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd2);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();

    // Verify the __updatedAt column exists with a default constraint
    let mut stmt = conn.prepare("PRAGMA table_info(Config)").unwrap();
    let iter = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1).unwrap(), // name
            row.get::<_, Option<String>>(4).unwrap() // dflt_value
        ))
    }).unwrap();

    let mut found = false;
    for result in iter {
        let (name, default) = result.unwrap();
        if name == "__updatedAt" {
            found = true;
            assert!(default.is_some(), "Expected default value for __updatedAt");
            assert_eq!(default.unwrap().to_uppercase(), "CURRENT_TIMESTAMP", "Default value should be CURRENT_TIMESTAMP");
        }
    }
    assert!(found, "Column __updatedAt was not added during migration.");

    // Verify the trigger was created
    let mut stmt = conn.prepare("SELECT sql FROM sqlite_master WHERE type='trigger' AND tbl_name='Config' AND name LIKE 'trg_update_Config___updatedAt'").unwrap();
    let sql: String = stmt.query_row([], |row| row.get(0)).unwrap();
    assert!(sql.contains("UPDATE Config SET __updatedAt = CURRENT_TIMESTAMP WHERE __id = OLD.__id;"), "Trigger sql does not contain expected update statement.");
}