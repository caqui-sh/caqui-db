use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;
use std::sync::Arc;
use ax_body::Body;
use axum::{body as ax_body, http::{self, Request, StatusCode}};
use tower::util::ServiceExt;
use serde_json::{json, Value};

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command {:?} failed!\nstdout: {}\nstderr: {}", cmd, stdout, stderr);
    }
    stdout
}

async fn setup_app(schema: &str) -> (axum::Router, tempfile::TempDir, String) {
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

    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    
    let pool = api_layer::db::create_pool(&db_uri);
    
    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool.clone()
    };
    let app = api_layer::router::build_dynamic_router(state);
    
    (app, dir, db_uri)
}

async fn post_query(app: &axum::Router, payload: Value) -> (StatusCode, Value) {
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/api/v1/query")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), 100000).await.unwrap();
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
        json!({ "error": String::from_utf8_lossy(&body_bytes) })
    });
    (status, body_val)
}

#[tokio::test]
async fn test_e2e_id_strategies() {
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
    let (app, _dir, _db_uri) = setup_app(schema).await;

    // Test CUID
    let payload = json!({
        "action": "create",
        "model": "CuidModel",
        "data": { "name": "CuidTest" }
    });
    
    let (status, response) = post_query(&app, payload).await;
    println!("CUID RESPONSE: {:?}", response);
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);
    let cuid = response["data"].as_str().or_else(|| response["data"]["__id"].as_str()).unwrap();
    assert!(cuid.len() > 10, "Cuid should be generated and have sufficient length, got: {}", cuid);
    
    // Test AutoIncrement
    let payload_auto = json!({
        "action": "create",
        "model": "AutoIncModel",
        "data": { "name": "AutoTest" }
    });
    
    let (status_auto, response_auto) = post_query(&app, payload_auto).await;
    println!("AUTOINC RESPONSE: {:?}", response_auto);
    assert_eq!(status_auto, StatusCode::OK, "Response: {:?}", response_auto);
    let auto_id = response_auto["data"].as_i64()
        .or_else(|| response_auto["data"]["__id"].as_i64())
        .or_else(|| response_auto["data"].as_str().and_then(|s| s.parse().ok()))
        .or_else(|| response_auto["data"]["__id"].as_str().and_then(|s| s.parse().ok()))
        .unwrap();
    assert_eq!(auto_id, 1, "Autoincrement should generate sequential IDs starting at 1");
}
