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
        enum PostStatus {
            DRAFT
            PUBLISHED
            ARCHIVED
        }

        model User {
            email: String @unique
            name: String
            posts: Post[]
            @@id(uuid)
        }
        
        model Post {
            title: String @unique
            authorId: String?
            tags: String[]
            history: PostStatus[]
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

async fn execute_query(app: &axum::Router, payload: Value) -> (StatusCode, String) {
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    let status = res.status();
    let body_bytes = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes).to_string();
    (status, body_str)
}

#[tokio::test]
async fn test_strict_type_validation_rejections() {
    let (app, _pool, _dir) = setup_app().await;

    // 1. Create a Post
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "create",
        "model": "Post",
        "data": { "title": "Validation Post" }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // 2. push invalid type into String[]
    let (status, _body) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Validation Post" },
        "data": {
            "tags": { "push": ["rust", 42] }
        }
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 3. malformed pullIndex
    let (status, body) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Validation Post" },
        "data": {
            "tags": { "pullIndex": [1, "two", 3] }
        }
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("pullIndex array must contain only integers"));

    // 4. EnumArray strictness
    let (status, body) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Validation Post" },
        "data": {
            "history": { "push": ["DRAFT", "INVALID_STATUS"] }
        }
    })).await;
    println!("ENUM STRICTNESS STATUS: {}, BODY: {}", status, body);
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("is not a valid variant for enum"));
}

#[tokio::test]
async fn test_nested_and_batch_contexts() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Create User with nested Posts
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "create",
        "model": "User",
        "data": {
            "email": "nested@test.com",
            "name": "Nested User",
            "posts": {
                "create": [
                    { "title": "P1", "tags": ["a", "b"] },
                    { "title": "P2", "tags": ["b", "c"] }
                ]
            }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // 2. Nested update with multi-pull
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "nested@test.com" },
        "data": {
            "posts": {
                "update": {
                    "where": { "title": "P1" },
                    "data": { "tags": { "pull": ["a"] } }
                }
            }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'P1'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"b\"]");
    }).await.unwrap();

    // 3. Nested updateMany with multi-push
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "nested@test.com" },
        "data": {
            "posts": {
                "updateMany": {
                    "where": {}, // Target all related posts
                    "data": { "tags": { "push": ["shared"] } }
                }
            }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    conn.interact(|db| {
        let tags_1: String = db.query_row("SELECT tags FROM Post WHERE title = 'P1'", [], |r| r.get(0)).unwrap();
        let tags_2: String = db.query_row("SELECT tags FROM Post WHERE title = 'P2'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags_1, "[\"b\",\"shared\"]");
        assert_eq!(tags_2, "[\"b\",\"c\",\"shared\"]");
    }).await.unwrap();
}

#[tokio::test]
async fn test_null_state_and_empty_coalescence() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Create a Post where tags is completely NULL
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "create",
        "model": "Post",
        "data": { "title": "Null Post" }
    })).await;
    assert_eq!(status, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: Option<String> = db.query_row("SELECT tags FROM Post WHERE title = 'Null Post'", [], |r| r.get(0)).unwrap();
        assert!(tags.is_none(), "Expected tags to be physically NULL initially");
    }).await.unwrap();

    // 2. Push onto NULL state (should coalesce to [])
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Null Post" },
        "data": {
            "tags": { "push": ["init"] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Null Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"init\"]");
    }).await.unwrap();

    // 3. Empty Payload Execution
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Null Post" },
        "data": {
            "tags": { "push": [] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);
    
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Null Post" },
        "data": {
            "tags": { "pull": [] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);
    
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Null Post" },
        "data": {
            "tags": { "pullIndex": [] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // Ensure array is unchanged
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Null Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"init\"]");
    }).await.unwrap();
}

#[tokio::test]
async fn test_index_out_of_bounds_and_missing_elements() {
    let (app, pool, _dir) = setup_app().await;

    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "create",
        "model": "Post",
        "data": { "title": "Bounds Post", "tags": ["a", "b", "c"] }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // 1. pull Ghost Elements
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Bounds Post" },
        "data": {
            "tags": { "pull": ["d", "e"] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Bounds Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"a\",\"b\",\"c\"]");
    }).await.unwrap();

    // 2. pullIndex Out of Bounds
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Bounds Post" },
        "data": {
            "tags": { "pullIndex": [0, 99] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Bounds Post'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, "[\"b\",\"c\"]"); // Only 'a' (index 0) was removed
    }).await.unwrap();
}

#[tokio::test]
async fn test_multi_pullindex_shifting_safety() {
    let (app, pool, _dir) = setup_app().await;

    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "create",
        "model": "Post",
        "data": { "title": "Shift Post", "tags": ["0", "1", "2", "3", "4", "5"] }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // 1. Out-of-Order Indices
    let (status, _) = execute_query(&app, serde_json::json!({
        "action": "update",
        "model": "Post",
        "where": { "title": "Shift Post" },
        "data": {
            "tags": { "pullIndex": [4, 1, 3] }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM Post WHERE title = 'Shift Post'", [], |r| r.get(0)).unwrap();
        // Indices 1, 3, and 4 correspond to values "1", "3", and "4".
        // The remaining array should be ["0", "2", "5"]
        assert_eq!(tags, "[\"0\",\"2\",\"5\"]");
    }).await.unwrap();
}
