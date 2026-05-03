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
async fn test_e2e_unique_constraints() {
    let schema = r#"
        model UniqueModel {
            email: String @unique
            @@id(uuid)
        }
    "#;
    let (app, dir, db_uri) = setup_app(schema).await;

    // First insertion should succeed
    let payload_1 = json!({
        "action": "create",
        "model": "UniqueModel",
        "data": { "email": "test@test.com" }
    });
    
    let (status1, _) = post_query(&app, payload_1).await;
    assert_eq!(status1, StatusCode::OK);
    
    // Second insertion should fail with Unique Constraint Violation
    let payload_2 = json!({
        "action": "create",
        "model": "UniqueModel",
        "data": { "email": "test@test.com" }
    });
    
    let (status2, response2) = post_query(&app, payload_2).await;
    assert_ne!(status2, StatusCode::OK, "Duplicate insertion should have failed");
    
    let err_msg = response2["error"].as_str().or(response2["message"].as_str()).unwrap_or("");
    assert!(err_msg.contains("UNIQUE constraint failed"), "Error should indicate a unique constraint violation, got: {}", err_msg);
    
    // Test Schema Evolution: Remove @unique
    let schema_v2 = r#"
        model UniqueModel {
            email: String
            @@id(uuid)
        }
    "#;
    let workspace = dir.path();
    fs::write(workspace.join("schema.cq"), schema_v2).unwrap();

    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);
    
    let pool2 = api_layer::db::create_pool(&db_uri);
    let ast_v2 = schema_parser::parser::parse_schema(schema_v2).unwrap();
    let ast_v2 = schema_parser::validation::validate_schema(ast_v2).unwrap();
    let state_v2 = api_layer::state::EngineState { 
        ast: Arc::new(ast_v2), 
        db_pool: pool2.clone()
    };
    let app_v2 = api_layer::router::build_dynamic_router(state_v2);
    
    // Second insertion should now succeed because @unique was removed
    let payload_3 = json!({
        "action": "create",
        "model": "UniqueModel",
        "data": { "email": "test@test.com" }
    });
    
    let (status3, _) = post_query(&app_v2, payload_3).await;
    assert_eq!(status3, StatusCode::OK, "Duplicate insertion should succeed after @unique is removed");
}
