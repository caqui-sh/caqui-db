use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command {:?} failed!\nstdout: {}\nstderr: {}", cmd, stdout, stderr);
    }
    stdout
}

#[tokio::test]
async fn test_e2e_id_strategies() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let _ = engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");

    let mut git_init = Command::new("git");
    git_init.arg("init").current_dir(workspace);
    run_cmd(git_init);

    let mut caqui_init = Command::new(caqui_bin);
    caqui_init.arg("init").current_dir(workspace);
    run_cmd(caqui_init);

    let schema = r#"
        model CuidModel {
            name: String
            @@id(cuid)
        }
        
        model AutoIncModel {
            name: String
            @@id(autoincrement)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    
    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let pool = api_layer::db::create_pool(&db_uri);
    
    // Test CUID
    let payload = serde_json::json!({
        "data": { "name": "CuidTest" }
    });
    
    let mut alias_idx = 0;
    let plan = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "CuidModel", "create", &payload, &mut alias_idx).unwrap();
    
    let response = api_layer::executor::execute_mutation_plan(&pool, plan).await.unwrap();
    
    // Verify it's generated string
    assert!(response.len() > 10, "Cuid should be generated and have sufficient length, got: {}", response);
    
    // Test AutoIncrement
    let payload_auto = serde_json::json!({
        "data": { "name": "AutoTest" }
    });
    
    // Refresh pool for new model
    let pool2 = api_layer::db::create_pool(&db_uri);
    
    let mut alias_idx = 0;
    let plan_auto = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "AutoIncModel", "create", &payload_auto, &mut alias_idx).unwrap();
    
    let response_auto = api_layer::executor::execute_mutation_plan(&pool2, plan_auto).await.unwrap();
    assert_eq!(response_auto, "1", "Autoincrement should generate sequential IDs starting at 1");
}
