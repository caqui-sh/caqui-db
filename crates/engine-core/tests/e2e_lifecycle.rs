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
    env::set_current_dir(workspace).unwrap();
    engine_core::vfs::bootstrap_custom_vfs();
    
    // We get the path to the compiled binary from cargo
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = "file:app.db?vfs=git";

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
    db_push.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(db_push);

    assert!(workspace.join("app.db").exists(), "app.db was not created");

    // 3. Database Mutation & Initial Commit
    // Modify DB natively via SQLite as mutations aren't via API yet.
    {
        let conn = rusqlite::Connection::open_with_flags(
            "file:app.db?vfs=git",
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (id, name) VALUES ('u1', 'Alice')", []).unwrap();
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
    schema = schema.replace("name  String", "name  String\n  status String @default(\"active\")");
    fs::write(&schema_path, schema).unwrap();

    let mut db_push_feature = Command::new(caqui_bin);
    db_push_feature.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(db_push_feature);

    // Insert new data on the feature branch
    {
        let conn = rusqlite::Connection::open_with_flags(
            "file:app.db?vfs=git",
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ).unwrap();
        conn.execute("INSERT INTO User (id, name, status) VALUES ('u2', 'Bob', 'pending')", []).unwrap();
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
        conn.execute("INSERT INTO User (id, name) VALUES ('u3', 'Charlie')", []).unwrap();
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
        "-s", "-X", "POST", "http://localhost:4000/api/v1/query",
        "-H", "Content-Type: application/json",
        "-d", r#"{"model":"User","action":"findMany","select":{"id":true,"name":true}}"#
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
