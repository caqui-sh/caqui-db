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
        model User {
            name: String
            age: Int
            status: String
            posts: Post[]
            @@id(uuid)
        }
        model Post {
            title: String
            published: Boolean
            authorId: String
            author: User 
            @@id(uuid)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    
    // Seed Data manually via rusqlite
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name, age, status) VALUES ('u1', 'Alice', 25, 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status) VALUES ('u2', 'Bob', 30, 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status) VALUES ('u3', 'Charlie', 22, 'inactive')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status) VALUES ('u4', 'Dave', 19, 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status) VALUES ('u5', 'Eve', 35, 'inactive')", []).unwrap();

        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p1', 'Alice Post 1', 1, 'u1')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p2', 'Alice Post 2', 0, 'u1')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p3', 'Bob Post 1', 1, 'u2')", []).unwrap();
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

#[tokio::test]
async fn test_query_filtering() {
    let (app, _dir) = setup_app().await;

    // Simple Filter: status == active
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "status": "active" },
        "select": { "name": true }
    });

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

    if response.status() != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
        panic!("Request failed with status {}: {:?}", StatusCode::BAD_REQUEST, String::from_utf8_lossy(&body_bytes));
    }
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let users = data["data"].as_array().expect("Response should contain 'data' array");
    assert_eq!(users.len(), 3); // Alice, Bob, Dave
    
    // Complex Filter: name == Bob AND status == active
    let complex_payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "status": "active", "name": "Bob" },
        "select": { "name": true, "age": true }
    });

    let response = app.clone()
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/api/v1/query")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(complex_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    if response.status() != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
        panic!("Request failed with status {}: {:?}", StatusCode::BAD_REQUEST, String::from_utf8_lossy(&body_bytes));
    }
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let users = data["data"].as_array().expect("Response should contain 'data' array");
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["name"], "Bob");
}

#[tokio::test]
async fn test_query_pagination_and_sorting() {
    let (app, _dir) = setup_app().await;

    // Test Sorting: orderBy name desc
    let sort_payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "orderBy": { "name": "desc" },
        "select": { "name": true }
    });

    let response = app.clone()
        .oneshot(Request::builder().method(http::Method::POST).uri("/api/v1/query").header(http::header::CONTENT_TYPE, "application/json").body(Body::from(sort_payload.to_string())).unwrap())
        .await.unwrap();

    if response.status() != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
        panic!("Request failed with status {}: {:?}", StatusCode::BAD_REQUEST, String::from_utf8_lossy(&body_bytes));
    }
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let users = data["data"].as_array().expect("Response should contain 'data' array");
    let names: Vec<_> = users.iter().map(|u| u["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Eve", "Dave", "Charlie", "Bob", "Alice"]);

    // Test Pagination: limit 2, skip 1 (Sorted by name asc: Alice, [Bob, Charlie], Dave, Eve)
    let pag_payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "orderBy": { "name": "asc" },
        "limit": 2,
        "skip": 1,
        "select": { "name": true }
    });

    let response = app.clone()
        .oneshot(Request::builder().method(http::Method::POST).uri("/api/v1/query").header(http::header::CONTENT_TYPE, "application/json").body(Body::from(pag_payload.to_string())).unwrap())
        .await.unwrap();

    if response.status() != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
        panic!("Request failed with status {}: {:?}", StatusCode::BAD_REQUEST, String::from_utf8_lossy(&body_bytes));
    }
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let users = data["data"].as_array().expect("Response should contain 'data' array");
    let names: Vec<_> = users.iter().map(|u| u["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Bob", "Charlie"]);
}

#[tokio::test]
async fn test_query_nested_reads() {
    let (app, _dir) = setup_app().await;

    // Fetch Alice and her published posts
    let nested_payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": "Alice" },
        "select": {
            "name": true,
            "posts": {
                "where": { "published": true },
                "select": { "title": true }
            }
        }
    });

    let response = app.clone()
        .oneshot(Request::builder().method(http::Method::POST).uri("/api/v1/query").header(http::header::CONTENT_TYPE, "application/json").body(Body::from(nested_payload.to_string())).unwrap())
        .await.unwrap();

    if response.status() != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
        panic!("Request failed with status {}: {:?}", StatusCode::BAD_REQUEST, String::from_utf8_lossy(&body_bytes));
    }
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 10000).await.unwrap();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();
    let users = data["data"].as_array().expect("Response should contain 'data' array");
    let alice = &users[0];
    
    assert_eq!(alice["name"], "Alice");
    let posts = alice["posts"].as_array().expect("Nested posts should be array");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["title"], "Alice Post 1");
}
