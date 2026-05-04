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

            name: String
            age: Int
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 3. Insert initial data
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name, age) VALUES ('u1', 'Alice', 20)", []).unwrap();
        conn.execute("INSERT INTO User (__id, name, age) VALUES ('u2', 'Bob', 25)", []).unwrap();
        conn.execute("INSERT INTO User (__id, name, age) VALUES ('u3', 'Charlie', 30)", []).unwrap();
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

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        // Drop Bob
        conn.execute("DELETE FROM User WHERE __id = 'u2'", []).unwrap();
        // Modify Alice
        conn.execute("UPDATE User SET name = 'Alice Updated', email = 'alice@test.com' WHERE __id = 'u1'", []).unwrap();
        // Update Charlie with email
        conn.execute("UPDATE User SET email = 'charlie@test.com' WHERE __id = 'u3'", []).unwrap();
        // Insert Dave
        conn.execute("INSERT INTO User (__id, name, email) VALUES ('u4', 'Dave', 'dave@test.com')", []).unwrap();
        
        // Insert Post
        conn.execute("INSERT INTO Post (__id, title) VALUES ('p1', 'Hello World')", []).unwrap();
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
    assert!(diff_text.contains("+ Inserted\x1b[0m (__id: p1)"), "Missing inserted post p1");
    assert!(diff_text.contains("+ Inserted\x1b[0m (__id: u4)"), "Missing inserted user u4");

    // Assert Record Changes - Deleted
    assert!(diff_text.contains("- Deleted \x1b[0m (__id: u2)"), "Missing deleted user u2");

    // Assert Record Changes - Modified
    assert!(diff_text.contains("~ Modified\x1b[0m (__id: u1)"), "Missing modified user u1");
    assert!(diff_text.contains("↳ name: \x1b[31mAlice\x1b[0m -> \x1b[32mAlice Updated\x1b[0m"), "Missing modified value for user u1");
    }

    #[test]
    fn test_e2e_git_diff_advanced() {
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

    // 2. Init caqui & create initial schema (v1)
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let initial_schema = "
        base Timestamped {
            createdAt: DateTime
        }

        enum Role {
            USER
            ADMIN
        }

        union Media = Image | Video

        model Content extends Timestamped {
            title: String
            role: Role
            media: Media
            tags: String[]

            @@id(uuid)
        }

        model Image {
            url: String
            @@id(uuid)
        }

        model Video {
            url: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 3. Insert baseline data
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        // Insert Image
        conn.execute("INSERT INTO Image (__id, url) VALUES ('img1', 'http://example.com/img1.png')", []).unwrap();
        // Insert Content (polymorphic relation, array, enum)
        conn.execute(
            "INSERT INTO Content (__id, title, role, media_type, media_id, tags, createdAt) VALUES ('c1', 'My First Post', 'USER', 'Image', 'img1', '[\"news\"]', '2024-01-01')", 
            []
        ).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "initial state v1"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Evolve the Schema (v2)
    let evolved_schema = "
        base Timestamped {
            createdAt: DateTime
            updatedAt: DateTime
        }

        enum Role {
            ADMIN
            MODERATOR
        }

        union Media = Image | Audio

        model Content extends Timestamped {
            title: String?
            role: Role
            media: Media
            tags: String[]
            author: User

            @@id(uuid)
        }

        model User {
            name: String
            @@id(uuid)
        }

        model Image {
            url: String
            @@id(uuid)
        }

        model Audio {
            url: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 5. Mutate data to match v2
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();

        // Insert User
        conn.execute("INSERT INTO User (__id, name) VALUES ('u1', 'Alice')", []).unwrap();
        // Insert Audio
        conn.execute("INSERT INTO Audio (__id, url) VALUES ('aud1', 'http://example.com/aud1.mp3')", []).unwrap();

        // Update Content c1 (role, media, tags, authorId)
        conn.execute(
            "UPDATE Content SET role = 'MODERATOR', media_type = 'Audio', media_id = 'aud1', tags = '[\"news\",\"update\"]', authorId = 'u1' WHERE __id = 'c1'", 
            []
        ).unwrap();
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

    // Bases
    assert!(diff_text.contains("+ Added field `updatedAt` (DateTime)"), "Missing Added field `updatedAt` in Base");

    // Enums
    assert!(diff_text.contains("- Dropped variant `USER`"), "Missing Dropped variant `USER` in Enum");
    assert!(diff_text.contains("+ Added variant `MODERATOR`"), "Missing Added variant `MODERATOR` in Enum");

    // Unions
    assert!(diff_text.contains("- Dropped variant `Video`"), "Missing Dropped variant `Video` in Union");
    assert!(diff_text.contains("+ Added variant `Audio`"), "Missing Added variant `Audio` in Union");

    // Models
    assert!(diff_text.contains("~ Changed `title` from String to String?"), "Missing changed title type");
    assert!(diff_text.contains("+ Added field `author` (User)"), "Missing added author relational field");
    assert!(diff_text.contains("+ Added Model `User`"), "Missing added User model");
    assert!(diff_text.contains("+ Added Model `Audio`"), "Missing added Audio model");
    assert!(diff_text.contains("- Dropped Model `Video`"), "Missing dropped Video model");

    // Assert Record Changes - Modified c1
    assert!(diff_text.contains("~ Modified\x1b[0m (__id: c1)"), "Missing modified content c1");
    assert!(diff_text.contains("↳ role: \x1b[31mUSER\x1b[0m -> \x1b[32mMODERATOR\x1b[0m"), "Missing role modification");
    assert!(diff_text.contains("↳ media_type: \x1b[31mImage\x1b[0m -> \x1b[32mAudio\x1b[0m"), "Missing media_type modification");
    assert!(diff_text.contains("↳ media_id: \x1b[31mimg1\x1b[0m -> \x1b[32maud1\x1b[0m"), "Missing media_id modification");
    assert!(diff_text.contains("↳ tags: \x1b[31m[\"news\"]\x1b[0m -> \x1b[32m[\"news\",\"update\"]\x1b[0m"), "Missing tags modification");
    }

#[test]
fn test_e2e_git_diff_attributes() {
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

    // 2. Init caqui & create initial schema (v1)
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let initial_schema = "
        base Timestamped {
            updatedAt: DateTime?
        }
        
        model Post extends Timestamped {
            title: String
            body: String
            
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), initial_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 3. Insert baseline data
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        
        conn.execute(
            "INSERT INTO Post (__id, title, body, updatedAt) VALUES ('p1', 'Hello', 'World', '2024-01-01 10:00:00')", 
            []
        ).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "initial state v1"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Evolve the Schema (v2)
    let evolved_schema = "
        base Timestamped {
            updatedAt: DateTime? @unique
        }
        
        model Post extends Timestamped {
            title: String @unique
            body: String
            
            @@id(uuid)
            @@track
            @@fulltext([title, body])
        }
    ";
    fs::write(workspace.join("schema.cq"), evolved_schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 5. Mutate data to match v2
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();

        // Update Post p1 (title and updatedAt due to tracking trigger equivalent effect for our manual query)
        conn.execute(
            "UPDATE Post SET title = 'Hello Update' WHERE __id = 'p1'", 
            []
        ).unwrap();
        // Since sqlite triggers for tracking will naturally update the `updatedAt` field upon UPDATE 
        // we just rely on our explicit UPDATE above to cause SQLite to fire the trigger we just pushed via schema.
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
    assert!(diff_text.contains("~ Changed `updatedAt` (DateTime?): Added @unique"), "Missing Base Added @unique");
    assert!(diff_text.contains("+ Added Model Attribute: @@track"), "Missing Added Model Attribute: @@track");
    assert!(diff_text.contains("+ Added Model Attribute: @@fulltext([title, body])"), "Missing Added Model Attribute: @@fulltext");
    assert!(diff_text.contains("~ Changed `title` (String): Added @unique"), "Missing Added @unique");

    // Assert Record Changes - Modified p1
    assert!(diff_text.contains("~ Modified\x1b[0m (__id: p1)"), "Missing modified post p1");
    assert!(diff_text.contains("↳ title: \x1b[31mHello\x1b[0m -> \x1b[32mHello Update\x1b[0m"), "Missing title modification");
}