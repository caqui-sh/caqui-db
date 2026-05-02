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

fn run_cmd_expect_error(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        panic!("Command was expected to fail but succeeded: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", cmd, stdout, stderr);
    }
    format!("{}\n{}", stdout, stderr)
}

#[test]
fn test_e2e_rejects_cyclic_inheritance() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let schema = r#"
        base Node extends Entity { @@id(uuid)}
        base Entity extends Node { @@id(uuid)}
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    let err_msg = run_cmd_expect_error(cmd);
    assert!(err_msg.contains("Circular inheritance detected among bases."), "Compiler failed to catch base inheritance cycle. Error: {}", err_msg);
}

#[test]
fn test_e2e_rejects_model_extending_model() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let schema = r#"
        model User { @@id(uuid) }
        model Admin extends User { role: String @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    let err_msg = run_cmd_expect_error(cmd);
    assert!(err_msg.contains("cannot extend 'User' because it is a model, not a base"), "Compiler allowed a model to extend a model. Error: {}", err_msg);
}

#[test]
fn test_e2e_rejects_bases_in_unions() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let schema = r#"
        base Timestamped { createdAt: String @@id(uuid) }
        model Task { @@id(uuid) }
        union SearchResult = Task | Timestamped
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    let err_msg = run_cmd_expect_error(cmd);
    assert!(err_msg.contains("references abstract base 'Timestamped', which is not allowed"), "Compiler allowed an abstract base inside a union. Error: {}", err_msg);
}

#[test]
fn test_e2e_polymorphic_unique_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let caqui_bin = env::var("CARGO_BIN_EXE_caqui").unwrap();
    engine_core::vfs::bootstrap_custom_vfs();

    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    // Setup schema with unique trait
    let schema = r#"
        base User { email: String @unique @@id(uuid) }
        model Admin extends User { role: String @@id(uuid) }
        model Customer extends User { balance: Int @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &db_uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ).unwrap();

    // The core of the test: Physical uniqueness is isolated per concrete model!
    // We should be able to insert 'alice@test.com' into both Admin and Customer because
    // abstract bases don't have global physical tables.
    
    conn.execute("INSERT INTO Admin (__id, email, role) VALUES ('1', 'alice@test.com', 'super')", []).expect("Failed to insert Admin");
    
    // This MUST succeed! If it fails with a Unique Constraint error, our DDL generation
    // incorrectly tried to enforce uniqueness globally via a shared table/index instead of per-model.
    conn.execute("INSERT INTO Customer (__id, email, balance) VALUES ('2', 'alice@test.com', 100)", []).expect("Failed to insert Customer with identical 'unique' trait value");

    // Sanity check: Inserting a second Admin with the same email MUST fail
    let res = conn.execute("INSERT INTO Admin (__id, email, role) VALUES ('3', 'alice@test.com', 'moderator')", []);
    assert!(res.is_err(), "Unique constraint failed to apply to the concrete table natively");
}
