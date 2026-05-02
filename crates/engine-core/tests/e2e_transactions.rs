use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;
use std::sync::Arc;
use axum::{body::Body, http::{self, Request, StatusCode}};
use tower::util::ServiceExt;
use serde_json::Value;

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command {:?} failed!\nstdout: {}\nstderr: {}", cmd, stdout, stderr);
    }
    stdout
}

async fn setup_app() -> (axum::Router, deadpool_sqlite::Pool, tempfile::TempDir) {
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
        model User {
            email: String @unique
            profile: Profile?
            @@id(uuid)
        }
        model Profile {
            bio: String
            userId: String
            user: User @relation(fields: [userId], references: [__id])
            @@id(uuid)
        }
    "#;
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
    
    (app, pool, dir)
}

#[tokio::test]
async fn test_atomic_rollback_nested_create() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Seed initial user
    let seed_payload = serde_json::json!({
        "action": "create",
        "model": "User",
        "data": { "email": "taken@test.com" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(seed_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. Attempt conflicting nested create
    // We try to create a User with same email AND a Profile.
    // The User creation should fail, and the Profile should NOT exist.
    let conflict_payload = serde_json::json!({
        "action": "create",
        "model": "User",
        "data": {
            "email": "taken@test.com",
            "profile": {
                "create": { "bio": "I should not exist" }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(conflict_payload.to_string())).unwrap()
    ).await.unwrap();
    
    // Should fail with 500 Internal Server Error (or 400 if specifically handled)
    assert!(res.status().is_client_error() || res.status().is_server_error());
    let body_bytes = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
    let err_msg = String::from_utf8_lossy(&body_bytes);
    println!("DEBUG ERROR MSG: {}", err_msg);
    assert!(err_msg.contains("UNIQUE constraint failed"), "Error message should mention unique constraint, got: {}", err_msg);

    // 3. Verify Rollback: Database should be clean of the failed operation
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let user_count: i64 = db.query_row("SELECT count(*) FROM User", [], |r| r.get(0)).unwrap();
        assert_eq!(user_count, 1, "Only the original user should exist");

        let profile_count: i64 = db.query_row("SELECT count(*) FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(profile_count, 0, "No profile should have been created (rolled back)");
    }).await.unwrap();
}
