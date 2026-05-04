use std::process::Command;
use std::env;
use std::fs;
use tempfile::tempdir;
use rusqlite::Connection;

// Helper to run commands
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

#[test]
fn test_e2e_migrate_dev() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // Write initial schema
    let initial_schema = "
        model User {

            name: String
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    // 2. First Migration
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    // Verify migrations directory exists
    let migrations_dir = workspace.join("migrations");
    assert!(migrations_dir.exists(), "migrations directory was not created");

    // Read the first migration file
    let entries: Vec<_> = fs::read_dir(&migrations_dir).unwrap().map(|res| res.unwrap().path()).collect();
    assert_eq!(entries.len(), 1, "Expected exactly one migration file");
    
    let migration_content = fs::read_to_string(&entries[0]).unwrap();
    println!("MIGRATION CONTENT:\n{}", migration_content);
    assert!(migration_content.contains("CREATE TABLE User"), "Migration should create the User table");

    // Verify app.db physically updated
    {
        let conn = Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='User'").unwrap();
        let table_exists = stmt.exists([]).unwrap();
        assert!(table_exists, "Table 'User' should exist in the live database");
    }

    // 3. Second Migration (Evolution)
    let evolved_schema = "
        model User {

            name: String
            email: String
    @@id(uuid)
        }
        
        model Post {

            title: String
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    // The migration files use second-level precision timestamps. 
    // Sleep for 1 second to ensure the second migration gets a new timestamp and doesn't overwrite the first.
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Run migrate again
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    println!("SECOND MIGRATE-DEV STDOUT:\n{}", stdout);
    println!("SECOND MIGRATE-DEV STDERR:\n{}", stderr);
    if !output.status.success() {
        panic!("Second migrate failed:\nSTDOUT:\n{}\nSTDERR:\n{}", stdout, stderr);
    }

    // Verify a second migration file was created
    let mut entries: Vec<_> = fs::read_dir(&migrations_dir).unwrap().map(|res| res.unwrap().path()).collect();
    println!("MIGRATION ENTRIES: {:?}", entries);
    assert_eq!(entries.len(), 2, "Expected exactly two migration files");
    
    // Sort to easily identify the latest one by timestamp prefix
    entries.sort();
    let second_migration_content = fs::read_to_string(&entries[1]).unwrap();
    println!("SECOND MIGRATION CONTENT:\n{}", second_migration_content);
    
    assert!(second_migration_content.contains("CREATE TABLE _engine_new_User"), "Second migration should rebuild the User table to add the email column");
    assert!(second_migration_content.contains("INSERT INTO _engine_new_User"), "Second migration should migrate data during rebuild");
    assert!(second_migration_content.contains("CREATE TABLE Post"), "Second migration should create the Post table");

    // Verify app.db physically updated
    {
        let conn = Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='Post'").unwrap();
        let post_exists = stmt.exists([]).unwrap();
        assert!(post_exists, "Table 'Post' should exist in the live database");
        
        // Verify 'email' column exists on 'User' via pragma
        let mut stmt = conn.prepare("PRAGMA table_info(User)").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut has_email = false;
        while let Some(row) = rows.next().unwrap() {
            let col_name: String = row.get(1).unwrap();
            if col_name == "email" {
                has_email = true;
                break;
            }
        }
        assert!(has_email, "Column 'email' should exist on the User table");
    }
}

#[test]
fn test_e2e_migrate_dev_advanced() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // Write initial schema
    let initial_schema = "
        model User {
            name: String
            bio: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    // 2. First Migration
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    // Verify migrations directory exists
    let migrations_dir = workspace.join("migrations");
    assert!(migrations_dir.exists(), "migrations directory was not created");

    // 3. Evolve Schema to v2
    let evolved_schema = "
        model User {
            name: String @unique
            posts: Post[]
            @@id(uuid)
            @@track
            @@fulltext([name])
        }

        model Post {
            title: String
            author: User
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    // The migration files use second-level precision timestamps. 
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Run migrate again
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    println!("SECOND MIGRATE-DEV STDOUT:\n{}", stdout);
    println!("SECOND MIGRATE-DEV STDERR:\n{}", stderr);
    if !output.status.success() {
        panic!("Second migrate failed:\nSTDOUT:\n{}\nSTDERR:\n{}", stdout, stderr);
    }

    // Verify a second migration file was created
    let mut entries: Vec<_> = fs::read_dir(&migrations_dir).unwrap().map(|res| res.unwrap().path()).collect();
    assert_eq!(entries.len(), 2, "Expected exactly two migration files");
    
    // Sort to easily identify the latest one by timestamp prefix
    entries.sort();
    let second_migration_content = fs::read_to_string(&entries[1]).unwrap();
    println!("SECOND MIGRATION CONTENT:\n{}", second_migration_content);
    
    // Assert Dropped Column / Rebuild
    assert!(second_migration_content.contains("CREATE TABLE _engine_new_User"), "Second migration should rebuild the User table");
    assert!(!second_migration_content.contains("bio TEXT"), "Rebuilt table should NOT contain the 'bio' column");
    assert!(second_migration_content.contains("name TEXT"), "Rebuilt table should contain the 'name' column");

    // Assert Attributes (Indexes)
    assert!(second_migration_content.contains("CREATE UNIQUE INDEX idx_User_name ON User (name);"), "Second migration should create unique index for 'name'");

    // Assert Attributes (Triggers)
    assert!(second_migration_content.contains("CREATE TRIGGER IF NOT EXISTS trg_update_User___updatedAt"), "Second migration should create tracking trigger for '__updatedAt'");

    // Assert Block Attributes (FullText)
    assert!(second_migration_content.contains("CREATE VIRTUAL TABLE IF NOT EXISTS User_fts USING fts5"), "Second migration should create FTS virtual table");
    assert!(second_migration_content.contains("CREATE TRIGGER IF NOT EXISTS User_fts_ai AFTER INSERT ON User"), "Second migration should create FTS insert trigger");
    assert!(second_migration_content.contains("CREATE TRIGGER IF NOT EXISTS User_fts_ad AFTER DELETE ON User"), "Second migration should create FTS delete trigger");
    assert!(second_migration_content.contains("CREATE TRIGGER IF NOT EXISTS User_fts_au AFTER UPDATE ON User"), "Second migration should create FTS update trigger");

    // Assert Relations
    assert!(second_migration_content.contains("CREATE TABLE Post"), "Second migration should create the Post table");
    assert!(second_migration_content.contains("FOREIGN KEY (authorId) REFERENCES \"User\" (__id) DEFERRABLE INITIALLY DEFERRED"), "Second migration should create foreign key for 'authorId'");
}

#[test]
fn test_e2e_migrate_cleanup_and_relations() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // Baseline schema (v1)
    let initial_schema = "
        model User {
            name: String
            @@id(uuid)
        }
        
        model Post {
            title: String
            @@fulltext([title])
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    std::thread::sleep(std::time::Duration::from_secs(1));

    // Evolve Schema (v2): Add relation to User, remove @@fulltext from Post.
    let evolved_schema = "
        model User {
            name: String
            posts: Post[]
            @@id(uuid)
        }
        
        model Post {
            title: String
            author: User
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    let migrations_dir = workspace.join("migrations");
    let mut entries: Vec<_> = fs::read_dir(&migrations_dir).unwrap().map(|res| res.unwrap().path()).collect();
    assert_eq!(entries.len(), 2);
    entries.sort();
    
    let second_migration_content = fs::read_to_string(&entries[1]).unwrap();
    println!("CLEANUP MIGRATION CONTENT:\n{}", second_migration_content);

    // Verify RebuildTable due to FK mutation
    assert!(second_migration_content.contains("CREATE TABLE _engine_new_Post"), "Should rebuild Post table to add FK");
    assert!(second_migration_content.contains("FOREIGN KEY (authorId) REFERENCES \"User\" (__id)"), "Rebuilt table should contain the new FK");

    // Verify virtual table cleanup 
    assert!(second_migration_content.contains("DROP TABLE IF EXISTS Post_fts;"), "Should drop orphaned FTS virtual table");
}

#[test]
fn test_e2e_migrate_dev_drop_table_and_index() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // Baseline schema (v1)
    let initial_schema = "
        model User {
            name: String @unique
            @@id(uuid)
        }
        
        model OldTable {
            data: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    std::thread::sleep(std::time::Duration::from_secs(1));

    // Evolve Schema (v2): Remove @unique, Drop OldTable
    let evolved_schema = "
        model User {
            name: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "migrate"]).current_dir(workspace);
    run_cmd(cmd);

    let migrations_dir = workspace.join("migrations");
    let mut entries: Vec<_> = fs::read_dir(&migrations_dir).unwrap().map(|res| res.unwrap().path()).collect();
    assert_eq!(entries.len(), 2);
    entries.sort();
    
    let second_migration_content = fs::read_to_string(&entries[1]).unwrap();
    println!("DROP MIGRATION CONTENT:\n{}", second_migration_content);

    // Verify DROP INDEX
    assert!(second_migration_content.contains("DROP INDEX idx_User_name;"), "Should drop the unique index for User(name)");

    // Verify DROP TABLE
    assert!(second_migration_content.contains("DROP TABLE OldTable;"), "Should drop OldTable completely");
}