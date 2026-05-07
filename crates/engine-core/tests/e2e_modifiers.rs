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
            name: String
            posts: Post[]
            @@id(uuid)
        }
        model Post {
            title: String
            authorId: String?
            tags: String[]
            author: User? 
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
async fn test_nested_connect_disconnect() {
    let (app, pool, _dir) = setup_app().await;

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": { "email": "u1@test.com", "name": "U1" }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
    let user_data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let user_id = user_data["data"]["__id"].as_str().expect("User data should have __id").to_string();

    // 2. Connect Post to User
    let connect_payload = serde_json::json!({
        "action": "create",
        "model": "Post",
        "data": {
            "title": "Connected Post",
            "author": {
                "set": { "__id": user_id }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(connect_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(move |db| {
        let fk: String = db.query_row("SELECT authorId FROM Post WHERE title = 'Connected Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, user_id);
    }).await.unwrap();

    // 3. Disconnect Post from User
    let disconnect_payload = serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Connected Post" },
        "data": {
            "author": {
                "set": null
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(disconnect_payload.to_string())).unwrap()
    ).await.unwrap();
    let status = res.status();
    let body_bytes = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    println!("DISCONNECT STATUS: {}, BODY: {}", status, body_str);
    assert_eq!(status, StatusCode::OK);

    conn.interact(|db| {
        let fk: Option<String> = db.query_row("SELECT authorId FROM Post WHERE title = 'Connected Post'", [], |r| r.get(0)).unwrap();
        assert!(fk.is_none());
    }).await.unwrap();
}

#[tokio::test]
async fn test_array_modifiers_multi_item() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Create a Post with an empty array
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "Post",
                "data": { "title": "Array Post" }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. Multi-Push
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "update",
                "model": "Post",
                "where": { "title": "Array Post" },
                "data": {
                    "tags": { "push": ["typescript", "rust"] }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Array Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"typescript\",\"rust\"]");
    }).await.unwrap();

    // Setup for pull
    app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "update",
                "model": "Post",
                "where": { "title": "Array Post" },
                "data": {
                    "tags": ["a", "b", "c", "b", "d"]
                }
            }).to_string())).unwrap()
    ).await.unwrap();

    // 3. Multi-Pull
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "update",
                "model": "Post",
                "where": { "title": "Array Post" },
                "data": {
                    "tags": { "pull": ["b", "c"] }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Array Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"a\",\"d\"]");
    }).await.unwrap();

    // Setup for pullIndex
    app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "update",
                "model": "Post",
                "where": { "title": "Array Post" },
                "data": {
                    "tags": ["0", "1", "2", "3", "4"]
                }
            }).to_string())).unwrap()
    ).await.unwrap();

    // 4. Multi-PullIndex
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "update",
                "model": "Post",
                "where": { "title": "Array Post" },
                "data": {
                    "tags": { "pullIndex": [1, 3] }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Array Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"0\",\"2\",\"4\"]");
    }).await.unwrap();
}
