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
async fn test_e2e_unique_constraints() {
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
        model UniqueModel {
            email: String @unique
            @@id(uuid)
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
    
    // First insertion should succeed
    let payload_1 = serde_json::json!({
        "data": { "email": "test@test.com" }
    });
    
    let mut alias_idx = 0;
    let plan_1 = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "UniqueModel", "create", &payload_1, &mut alias_idx).unwrap();
    
    let _ = api_layer::executor::execute_mutation_plan(&pool, plan_1).await.unwrap();
    
    // Second insertion should fail with Unique Constraint Violation
    let payload_2 = serde_json::json!({
        "data": { "email": "test@test.com" }
    });
    
    let mut alias_idx = 0;
    let plan_2 = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "UniqueModel", "create", &payload_2, &mut alias_idx).unwrap();
    
    let res = api_layer::executor::execute_mutation_plan(&pool, plan_2).await;
    
    assert!(res.is_err(), "Duplicate insertion should have failed");
    let err_msg = res.unwrap_err();
    assert!(err_msg.contains("UNIQUE constraint failed"), "Error should indicate a unique constraint violation, got: {}", err_msg);
    
    // Test Schema Evolution: Remove @unique
    let schema_v2 = r#"
        model UniqueModel {
            email: String
            @@id(uuid)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema_v2).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);
    
    let ast_v2 = schema_parser::parser::parse_schema(schema_v2).unwrap();
    let ast_v2 = schema_parser::validation::validate_schema(ast_v2).unwrap();
    
    // Create NEW connection pool to bypass cached connection issues or wal locks
    let pool2 = api_layer::db::create_pool(&db_uri);
    
    // Second insertion should now succeed because @unique was removed
    let payload_3 = serde_json::json!({
        "data": { "email": "test@test.com" }
    });
    
    let mut alias_idx = 0;
    let plan_3 = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast_v2, "UniqueModel", "create", &payload_3, &mut alias_idx).unwrap();
    
    let res_v2 = api_layer::executor::execute_mutation_plan(&pool2, plan_3).await;
    
    assert!(res_v2.is_ok(), "Duplicate insertion should succeed after @unique is removed, got: {:?}", res_v2.err());
}
