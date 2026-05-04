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
            name: String @unique
            age: Int
            bio: String
            role: String
            posts: Post[]
            @@id(uuid)
        }
        model Post {
            title: String
            authorId: String?
            author: User? @relation(onDelete: Cascade)
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
    
    // Seed DB
    let conn = pool.get().await.unwrap();
    conn.interact(|db| -> Result<(), rusqlite::Error> {
        // 3 Admins
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u1', 'Admin1', 30, 'bio', 'Admin');", [])?;
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u2', 'Admin2', 31, 'bio', 'Admin');", [])?;
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u3', 'Admin3', 32, 'bio', 'Admin');", [])?;
        
        // 2 Users
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u4', 'User1', 20, 'bio', 'User');", [])?;
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u5', 'User2', 21, 'bio', 'User');", [])?;
        
        // 1 Guest
        db.execute("INSERT INTO User (__id, name, age, bio, role) VALUES ('u6', 'Guest1', 18, 'bio', 'Guest');", [])?;
        
        // Posts
        db.execute("INSERT INTO Post (__id, title, authorId) VALUES ('p1', 'Admin Post', 'u1');", [])?;
        db.execute("INSERT INTO Post (__id, title, authorId) VALUES ('p2', 'User Post', 'u4');", [])?;
        db.execute("INSERT INTO Post (__id, title, authorId) VALUES ('p3', 'Another User Post', 'u4');", [])?;
        Ok(())
    }).await.unwrap().unwrap();

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
async fn test_update_many() {
    let (app, pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "role": "Admin" },
        "data": { "bio": "System Administrator" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json_body["data"]["count"], 3);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM User WHERE bio = 'System Administrator'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn test_delete_many() {
    let (app, pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "deleteMany",
        "where": { "role": "User" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json_body["data"]["count"], 2);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM User WHERE role = 'User'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_bulk_no_match() {
    let (app, _pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "role": "SuperUser" },
        "data": { "bio": "Hacked" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json_body["data"]["count"], 0);
}

#[tokio::test]
async fn test_bulk_unsupported_nested() {
    let (app, _pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "role": "Guest" },
        "data": { "posts": { "create": [{ "title": "New" }] } }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("Nested mutations are not supported"));
}

#[tokio::test]
async fn test_bulk_relational_filtering() {
    let (app, pool, _dir) = setup_app().await;

    // Update users with a specific post
    let update_payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "posts": { "some": { "title": "Admin Post" } } },
        "data": { "bio": "Author of Admin Post" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(update_payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json_body["data"]["count"], 1);

    let conn = pool.get().await.unwrap();
    let bio: String = conn.interact(|db| {
        db.query_row("SELECT bio FROM User WHERE __id = 'u1'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(bio, "Author of Admin Post");

    // Delete users with no posts
    let delete_payload = serde_json::json!({
        "model": "User",
        "action": "deleteMany",
        "where": { "posts": { "none": {} } }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(delete_payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    // Users u2, u3, u5, u6 have no posts (4 users)
    assert_eq!(json_body["data"]["count"], 4);
}

#[tokio::test]
async fn test_bulk_cascading_deletes() {
    let (app, pool, _dir) = setup_app().await;

    // Delete User1 (u4), which should cascade and delete p2 and p3
    let payload = serde_json::json!({
        "model": "User",
        "action": "deleteMany",
        "where": { "name": "User1" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM Post WHERE authorId = 'u4'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 0, "SQLite ON DELETE CASCADE should have fired and deleted the associated posts");
}

#[tokio::test]
async fn test_bulk_complex_scalar_filters() {
    let (app, pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { 
            "age": { "gt": 20, "lt": 32 }, 
            "role": "Admin" 
        },
        "data": { "bio": "Middle Admin" }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    // Only Admin1 (30) and Admin2 (31) match. Admin3 is 32, so it fails the lt: 32 condition.
    assert_eq!(json_body["data"]["count"], 2);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM User WHERE bio = 'Middle Admin'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_bulk_empty_where() {
    let (app, pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "deleteMany",
        "where": {}
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    // All 6 seeded users
    assert_eq!(json_body["data"]["count"], 6);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM User", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_bulk_multi_field_updates() {
    let (app, pool, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "role": "Admin" },
        "data": { 
            "bio": "Super Admin",
            "age": 99
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json_body["data"]["count"], 3);

    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM User WHERE bio = 'Super Admin' AND age = 99", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 3);
}