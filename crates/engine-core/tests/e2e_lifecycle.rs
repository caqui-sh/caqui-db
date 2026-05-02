use std::process::{Command, Stdio};
use std::env;
use std::fs;
use tempfile::tempdir;
use std::time::Duration;
use std::thread;

// Helper to run commands
fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    println!("CMD: {:?}\nSTDOUT: {}\nSTDERR: {}", cmd, stdout, stderr);
    if !output.status.success() {
        panic!("Command failed: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", 
               cmd, 
               stdout,
               stderr);
    }
    stdout
}

#[test]
fn test_e2e_lifecycle() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    // We get the path to the compiled binary from cargo
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Setup Ephemeral Environment
    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    let mut git_config_name = Command::new("git");
    git_config_name.args(&["config", "user.name", "E2E Test"]).current_dir(workspace);
    run_cmd(git_config_name);

    let mut git_config_email = Command::new("git");
    git_config_email.args(&["config", "user.email", "test@example.com"]).current_dir(workspace);
    run_cmd(git_config_email);

    // Get the default branch name
    let mut branch_cmd = Command::new("git");
    branch_cmd.args(&["branch", "--show-current"]).current_dir(workspace);
    let _ = run_cmd(branch_cmd); // Check current branch

    // In new git repos with no commits, show-current returns empty.
    // Let's just create an initial empty commit to establish the branch.
    let mut initial_commit = Command::new("git");
    initial_commit.args(&["commit", "--allow-empty", "-m", "root"]).current_dir(workspace);
    run_cmd(initial_commit);
    
    let mut branch_cmd = Command::new("git");
    branch_cmd.args(&["branch", "--show-current"]).current_dir(workspace);
    let default_branch = run_cmd(branch_cmd).trim().to_string();

    // 2. Project Initialization & Schema Push
    let mut caqui_init = Command::new(caqui_bin);
    caqui_init.arg("init").current_dir(workspace);
    run_cmd(caqui_init);

    assert!(workspace.join("schema.cq").exists(), "schema.cq was not created");

    let mut db_push = Command::new(caqui_bin);
    db_push.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(db_push);

    assert!(workspace.join("app.db").exists(), "app.db was not created");

    // 3. Database Mutation & Initial Commit
    // Modify DB natively via SQLite as mutations aren't via API yet.
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name) VALUES ('u1', 'Alice')", []).unwrap();
    }

    let mut git_add = Command::new(caqui_bin);
    git_add.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(git_add);

    let mut git_commit = Command::new(caqui_bin);
    git_commit.args(&["git", "commit", "-m", "initial schema"]).current_dir(workspace);
    run_cmd(git_commit);

    // 4. Branching & Schema Evolution
    let mut git_checkout = Command::new(caqui_bin);
    git_checkout.args(&["git", "checkout", "-b", "feature"]).current_dir(workspace);
    run_cmd(git_checkout);

    // Read and mutate the generated schema.cq
    let schema_path = workspace.join("schema.cq");
    let mut schema = fs::read_to_string(&schema_path).unwrap();
    schema = schema.replace("name: String", "name: String\n  status: String @default(\"active\")");
    fs::write(&schema_path, schema).unwrap();

    let mut db_push_feature = Command::new(caqui_bin);
    db_push_feature.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(db_push_feature);

    // Insert new data on the feature branch
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name, status) VALUES ('u2', 'Bob', 'pending')", []).unwrap();
    }

    let mut git_add_feature = Command::new(caqui_bin);
    git_add_feature.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(git_add_feature);

    let mut git_commit_feature = Command::new(caqui_bin);
    git_commit_feature.args(&["git", "commit", "-m", "feature update"]).current_dir(workspace);
    run_cmd(git_commit_feature);

    // 5. Divergent Main Branch
    let mut git_checkout_main = Command::new(caqui_bin);
    git_checkout_main.args(&["git", "checkout", &default_branch]).current_dir(workspace);
    run_cmd(git_checkout_main);

    // Insert data to cause database divergence on main
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name) VALUES ('u3', 'Charlie')", []).unwrap();
    }

    let mut git_add_main = Command::new(caqui_bin);
    git_add_main.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(git_add_main);

    let mut git_commit_main = Command::new(caqui_bin);
    git_commit_main.args(&["git", "commit", "-m", "main update"]).current_dir(workspace);
    run_cmd(git_commit_main);

    // 6. Custom Driver Merge Extraction & Check
    // We force `caqui git status` just to trigger the silent extraction of the binary.
    let mut trigger_extraction = Command::new(caqui_bin);
    trigger_extraction.args(&["git", "status"]).current_dir(workspace);
    run_cmd(trigger_extraction);

    let driver_path = workspace.join(".caqui").join("bin").join("git-merge-sqlitevfs");
    if let Ok(metadata) = fs::metadata(&driver_path) {
        if metadata.len() == 0 {
            println!("Skipping merge test as the downloaded custom driver is a 0-byte dummy file.");
            return; // Architecture not supported by pre-compiled binary
        }
    } else {
        panic!("Merge driver was not extracted!");
    }

    // Attempt the merge
    // The `caqui git merge` proxy automatically injects `-s sqlitevfs`
    let mut git_merge = Command::new(caqui_bin);
    git_merge.args(&["git", "merge", "feature"]).current_dir(workspace);
    let output = git_merge.output().expect("Failed to execute git merge");
    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        // If git-merge-sqlitevfs isn't available or fails, panic
        panic!("caqui git merge failed! See output above.");
    }

    // 7. API State Assertion
    let mut api_server = Command::new(caqui_bin)
        .env("PORT", "4001")
        .args(&["api", "start"])
        .current_dir(workspace)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Wait for the server to spin up and bind the port
    thread::sleep(Duration::from_secs(2));

    let mut curl_cmd = Command::new("curl");
    curl_cmd.args(&[
        "-s", "-X", "POST", "http://localhost:4001/api/v1/query",
        "-H", "Content-Type: application/json",
        "-d", r#"{"model":"User","action":"findMany","select":{"__id":true,"name":true}}"#
    ]);

    let curl_output = curl_cmd.output().expect("Failed to execute curl");
    let json_resp = String::from_utf8_lossy(&curl_output.stdout);

    // Gracefully terminate the API server
    api_server.kill().unwrap();
    api_server.wait().unwrap();

    // Assert merged state contains the combined users!
    let parsed: serde_json::Value = serde_json::from_str(&json_resp).expect("Failed to parse JSON response");
    let users = parsed["data"].as_array().expect("Expected data to be a JSON array");
    
    let mut found_alice = false;
    let mut found_bob = false;
    let mut found_charlie = false;
    
    for user in users {
        if let Some(name) = user["name"].as_str() {
            match name {
                "Alice" => found_alice = true,
                "Bob" => found_bob = true,
                "Charlie" => found_charlie = true,
                _ => {}
            }
        }
    }
    
    assert!(found_alice, "Missing Alice in merged API state. Response: {}", json_resp);
    assert!(found_bob, "Missing Bob in merged API state. Response: {}", json_resp);
    assert!(found_charlie, "Missing Charlie in merged API state. Response: {}", json_resp);
}

#[test]
fn test_e2e_hard_merge_conflict() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

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

    let mut cmd = Command::new("git");
    cmd.args(&["branch", "--show-current"]).current_dir(workspace);
    let default_branch = run_cmd(cmd).trim().to_string();

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name) VALUES ('u1', 'Alice')", []).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "initial"]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "checkout", "-b", "feature"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("UPDATE User SET name = 'Bob' WHERE __id = 'u1'", []).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "feature update"]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "checkout", &default_branch]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("UPDATE User SET name = 'Charlie' WHERE __id = 'u1'", []).unwrap();
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "add", "."]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "commit", "-m", "main update"]).current_dir(workspace);
    run_cmd(cmd);

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "status"]).current_dir(workspace);
    run_cmd(cmd);

    let driver_path = workspace.join(".caqui").join("bin").join("git-merge-sqlitevfs");
    if let Ok(metadata) = fs::metadata(&driver_path) {
        if metadata.len() == 0 {
            return;
        }
    }

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["git", "merge", "feature"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "Merge should fail with a conflict due to concurrent updates on the same row");
}

#[test]
fn test_e2e_complex_graph_traversal() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model User {
            name: String
            posts: Post[]
    @@id(uuid)
        }
        model Post {
            title: String
            user: User @relation
            comments: Comment[]
    @@id(uuid)
        }
        model Comment {
            body: String
            post: Post @relation
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (__id, name) VALUES ('u1', 'Alice')", []).unwrap();
        conn.execute("INSERT INTO Post (__id, title, userId) VALUES ('p1', 'First Post', 'u1')", []).unwrap();
        conn.execute("INSERT INTO Comment (__id, body, postId) VALUES ('c1', 'Nice post!', 'p1')", []).unwrap();
    }

    let mut api_server = Command::new(caqui_bin).env("PORT", "4002").args(&["api", "start"]).current_dir(workspace).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    thread::sleep(Duration::from_secs(2));

    let curl_output = Command::new("curl").args(&[
        "-s", "-X", "POST", "http://localhost:4002/api/v1/query",
        "-H", "Content-Type: application/json",
        "-d", r#"{"model":"User","action":"findMany","select":{"__id":true,"name":true,"posts":{"select":{"__id":true,"title":true,"comments":{"select":{"__id":true,"body":true}}}}}}"#
    ]).output().unwrap();

    api_server.kill().unwrap();
    api_server.wait().unwrap();

    let json_resp = String::from_utf8_lossy(&curl_output.stdout);
    println!("API RESPONSE: {}", json_resp);
    let parsed: serde_json::Value = serde_json::from_str(&json_resp).unwrap();
    let users = parsed["data"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    
    let posts = users[0]["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["title"], "First Post");

    let comments = posts[0]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "Nice post!");
}

#[test]
fn test_e2e_cli_misconfigurations() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    // 1. push without schema.cq
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    assert!(!output.status.success());

    // 2. api start without app.db
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["api", "start"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    assert!(!output.status.success());

    // 3. invalid schema
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);
    
    fs::write(workspace.join("schema.cq"), "invalid schema syntax").unwrap();
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    let output = cmd.output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn test_e2e_custom_functions_and_triggers() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model Item {
            name: String
            @@track
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        // Register custom functions manually here so the INSERT works natively
        api_layer::db::register_custom_functions(&conn).unwrap();
        
        conn.execute("INSERT INTO Item (__id, name) VALUES ('item_1', 'Test Item')", []).unwrap();
        
        let initial_updated_at: String = conn.query_row("SELECT __updatedAt FROM Item WHERE __id = 'item_1'", [], |r| r.get(0)).unwrap();
        
        std::thread::sleep(Duration::from_secs(1));
        
        conn.execute("UPDATE Item SET name = 'Updated Item' WHERE __id = 'item_1'", []).unwrap();
        
        let new_updated_at: String = conn.query_row("SELECT __updatedAt FROM Item WHERE __id = 'item_1'", [], |r| r.get(0)).unwrap();
        
        assert!(new_updated_at > initial_updated_at, "Temporal progression failed: {} is not greater than {}", new_updated_at, initial_updated_at);
    }

    let mut api_server = Command::new(caqui_bin).env("PORT", "4003").args(&["api", "start"]).current_dir(workspace).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    thread::sleep(Duration::from_secs(2));

    let curl_output = Command::new("curl").args(&[
        "-s", "-X", "POST", "http://localhost:4003/api/v1/query",
        "-H", "Content-Type: application/json",
        "-d", r#"{"model":"Item","action":"findMany","select":{"__id":true,"name":true,"__updatedAt":true}}"#
    ]).output().unwrap();

    api_server.kill().unwrap();
    api_server.wait().unwrap();

    let json_resp = String::from_utf8_lossy(&curl_output.stdout);
    println!("API RESPONSE: {}", json_resp);
    let parsed: serde_json::Value = serde_json::from_str(&json_resp).unwrap();
    let items = parsed["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    
    let __id = items[0]["__id"].as_str().unwrap();
    let updated_at = items[0]["__updatedAt"].as_str().unwrap();

    assert_eq!(__id, "item_1");
    assert!(!updated_at.is_empty());
}

#[test]
fn test_e2e_field_level_track() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model Profile {
            bio: String @track
            location: String
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();
    
    // Register custom functions manually here so the INSERT works natively
    api_layer::db::register_custom_functions(&conn).unwrap();
    
    conn.execute("INSERT INTO Profile (__id, bio, location) VALUES ('prof_1', 'Initial bio', 'NY')", []).unwrap();
    
    let initial_bio_updated_at: String = conn.query_row("SELECT __bio_updatedAt FROM Profile WHERE __id = 'prof_1'", [], |r| r.get(0)).unwrap();
    
    std::thread::sleep(Duration::from_secs(1));
    
    // Update location (bio should NOT update)
    conn.execute("UPDATE Profile SET location = 'SF' WHERE __id = 'prof_1'", []).unwrap();
    
    let bio_updated_at_after_loc_change: String = conn.query_row("SELECT __bio_updatedAt FROM Profile WHERE __id = 'prof_1'", [], |r| r.get(0)).unwrap();
    
    assert_eq!(bio_updated_at_after_loc_change, initial_bio_updated_at, "bio_updatedAt changed when it shouldn't have");

    std::thread::sleep(Duration::from_secs(1));
    
    // Update bio (bio SHOULD update)
    conn.execute("UPDATE Profile SET bio = 'New bio' WHERE __id = 'prof_1'", []).unwrap();
    
    let bio_updated_at_after_bio_change: String = conn.query_row("SELECT __bio_updatedAt FROM Profile WHERE __id = 'prof_1'", [], |r| r.get(0)).unwrap();
    
    assert!(bio_updated_at_after_bio_change > initial_bio_updated_at, "bio_updatedAt did not progress when bio was updated");
}

#[test]
fn test_e2e_field_level_track_edge_cases() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let mut cmd = Command::new("git");
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model User {
            @@id(uuid)
        }

        model ComplexModel {
            @@id(uuid)
            status: String @track
            bio: String @track @map(\"user_bio\")
            
            authorId: String?
            author: User? @relation(fields: [authorId], references: [__id]) @track
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();
    
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());
    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();

    // Verify Triggers are generated correctly with correct columns
    let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type='trigger' AND tbl_name='ComplexModel' ORDER BY name").unwrap();
    let triggers: Vec<(String, String)> = stmt.query_map([], |row| Ok((row.get(0).unwrap(), row.get(1).unwrap()))).unwrap().map(|r| r.unwrap()).collect();
    
    println!("TRIGGERS GENERATED: {:#?}", triggers);
    assert_eq!(triggers.len(), 3);

    // Verify Author Relation Track uses authorId
    let trg_author = triggers.iter().find(|(n, _)| n == "trg_update_ComplexModel___author_updatedAt").unwrap();
    assert!(trg_author.1.contains("AFTER UPDATE OF authorId ON ComplexModel"));
    
    // Verify Bio Map Track uses user_bio
    let trg_bio = triggers.iter().find(|(n, _)| n == "trg_update_ComplexModel___bio_updatedAt").unwrap();
    assert!(trg_bio.1.contains("AFTER UPDATE OF user_bio ON ComplexModel"));

    // Verify Status Track uses status
    let trg_status = triggers.iter().find(|(n, _)| n == "trg_update_ComplexModel___status_updatedAt").unwrap();
    assert!(trg_status.1.contains("AFTER UPDATE OF status ON ComplexModel"));
}
