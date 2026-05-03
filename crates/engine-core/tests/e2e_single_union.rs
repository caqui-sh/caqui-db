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

async fn setup_app() -> (axum::Router, tempfile::TempDir) {
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
        model Post {
            title: String
            @@id(uuid)
        }
        model Video {
            url: String
            @@id(uuid)
        }
        union Content = Post | Video
        model User {
            content: Content?
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
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Post (__id, title) VALUES ('p1', 'Hello World')", []).unwrap();
        db.execute("INSERT INTO Video (__id, url) VALUES ('v1', 'http://example.com/video')", []).unwrap();
        
        // User 1 points to a Post
        db.execute("INSERT INTO User (__id, content_type, content_id) VALUES ('u1', 'Post', 'p1')", []).unwrap();
        
        // User 2 points to a Video
        db.execute("INSERT INTO User (__id, content_type, content_id) VALUES ('u2', 'Video', 'v1')", []).unwrap();
        
        // User 3 points to NULL (empty)
        db.execute("INSERT INTO User (__id, content_type, content_id) VALUES ('u3', NULL, NULL)", []).unwrap();

        // User 4 points to an unknown discriminator
        db.execute("INSERT INTO User (__id, content_type, content_id) VALUES ('u4', 'UnknownType', '99')", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);
    
    (app, dir)
}

async fn post_query(app: &axum::Router, payload: Value) -> Value {
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
    if status != StatusCode::OK {
        panic!("Request failed with status {}: {:?}", status, String::from_utf8_lossy(&body_bytes));
    }
    serde_json::from_slice(&body_bytes).unwrap()
}

#[tokio::test]
async fn test_e2e_single_union_standard_hydration() {
    let (app, _dir) = setup_app().await;
    
    // Test Post hydration
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "content": {
                "Post": { "select": { "title": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u1");
    assert_eq!(user["content"]["title"], "Hello World");

    // Test Video hydration
    let payload2 = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u2" },
        "select": {
            "__id": true,
            "content": {
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response2 = post_query(&app, payload2).await;
    let users2 = response2["data"].as_array().unwrap();
    let user2 = &users2[0];

    assert_eq!(user2["__id"], "u2");
    assert_eq!(user2["content"]["url"], "http://example.com/video");
}

#[tokio::test]
async fn test_e2e_single_union_empty_state() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u3" },
        "select": {
            "__id": true,
            "content": {
                "Post": { "select": { "title": true } },
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u3");
    assert!(user["content"].is_null());
}

#[tokio::test]
async fn test_e2e_single_union_legacy_discriminator() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u4" },
        "select": {
            "__id": true,
            "content": {
                "Post": { "select": { "title": true } },
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u4");
    assert!(user["content"].is_null()); // Should gracefully fallback to NULL
}
