use std::process::Command;
use std::env;
use std::fs;
use tempfile::tempdir;

// Helper to run commands
fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() && cmd.get_args().any(|a| a == "diff") {
        // Diff returns 1 when there are differences, so don't panic
        return stdout;
    } else if !output.status.success() {
        panic!("Command failed: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", 
               cmd, 
               stdout,
               stderr);
    }
    stdout
}

#[test]
fn test_e2e_git_diff() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Setup git
    let mut cmd = Command::new("git");
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new("git");
    cmd.args(&["config", "user.name", "E2E Test"]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new("git");
    cmd.args(&["config", "user.email", "test@example.com"]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new("git");
    cmd.args(&["commit", "--allow-empty", "-m", "root"]).current_dir(workspace);
    run_cmd(cmd);

    // 2. Init caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let initial_schema = "
        model User {
            id String @id
            name String
            age Int
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    // 3. Insert initial data
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (id, name, age) VALUES ('u1', 'Alice', 20)", []).unwrap();
        conn.execute("INSERT INTO User (id, name, age) VALUES ('u2', 'Bob', 25)", []).unwrap();
        conn.execute("INSERT INTO User (id, name, age) VALUES ('u3', 'Charlie', 30)", []).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "initial state"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Check clean state baseline
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "diff", "HEAD"]).current_dir(workspace);
    let diff_out = cmd.output().unwrap();
    assert_eq!(diff_out.status.code(), Some(0), "Expected clean diff to exit 0");
    let diff_text = String::from_utf8_lossy(&diff_out.stdout);
    assert!(diff_text.contains("No differences found."));

    // 5. Mutate schema and data
    let evolved_schema = "
        model User {
            id String @id
            name String
            email String
        }
        model Post {
            id String @id
            title String
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        // Drop Bob
        conn.execute("DELETE FROM User WHERE id = 'u2'", []).unwrap();
        // Modify Alice
        conn.execute("UPDATE User SET name = 'Alice Updated', email = 'alice@test.com' WHERE id = 'u1'", []).unwrap();
        // Update Charlie with email
        conn.execute("UPDATE User SET email = 'charlie@test.com' WHERE id = 'u3'", []).unwrap();
        // Insert Dave
        conn.execute("INSERT INTO User (id, name, email) VALUES ('u4', 'Dave', 'dave@test.com')", []).unwrap();
        
        // Insert Post
        conn.execute("INSERT INTO Post (id, title) VALUES ('p1', 'Hello World')", []).unwrap();
    }

    // 6. Test diffing against HEAD
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "diff", "HEAD"]).current_dir(workspace);
    let diff_out = cmd.output().unwrap();
    assert_eq!(diff_out.status.code(), Some(1), "Expected differences to exit 1");
    
    let diff_text = String::from_utf8_lossy(&diff_out.stdout);
    let diff_err = String::from_utf8_lossy(&diff_out.stderr);
    println!("DIFF OUTPUT:\n{}", diff_text);
    println!("DIFF ERROR:\n{}", diff_err);

    // Assert Schema Changes
    assert!(diff_text.contains("+ Added Model `Post`"), "Missing Added Model `Post`");
    assert!(diff_text.contains("- Dropped field `age`"), "Missing Dropped field `age`");
    assert!(diff_text.contains("+ Added field `email` (String)"), "Missing Added field `email`");

    // Assert Record Changes - Inserted
    assert!(diff_text.contains("+ Inserted\x1b[0m (id: p1)"), "Missing inserted post p1");
    assert!(diff_text.contains("+ Inserted\x1b[0m (id: u4)"), "Missing inserted user u4");

    // Assert Record Changes - Deleted
    assert!(diff_text.contains("- Deleted \x1b[0m (id: u2)"), "Missing deleted user u2");

    // Assert Record Changes - Modified
    assert!(diff_text.contains("~ Modified\x1b[0m (id: u1)"), "Missing modified user u1");
    assert!(diff_text.contains("↳ name: \x1b[31mAlice\x1b[0m -> \x1b[32mAlice Updated\x1b[0m"), "Missing modified value for user u1");
}