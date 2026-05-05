use tempfile::tempdir;
use std::process::Command;
use std::fs;
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
            name: String
            email: String
            posts: Post[]
            @@id(uuid)
        }
        model Post {
            title: String
            authorId: String?
            author: User?
            comments: Comment[]
            @@id(uuid)
        }
        model Comment {
            content: String
            postId: String?
            post: Post?
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
        ast: std::sync::Arc::new(ast),
        db_pool: pool.clone()
    };
    let app = api_layer::router::build_dynamic_router(state);
    (app, pool, dir)
}

#[tokio::test]
async fn test_deeply_nested_bulk_mutations() {
    let (app, pool, _dir) = setup_app().await;

    // Create initial data via API
    let setup_payload = serde_json::json!({
        "model": "User",
        "action": "create",
        "data": {
            "__id": "u100",
            "name": "Nested User",
            "email": "nested@example.com",
            "posts": {
                "create": [{
                    "__id": "p100",
                    "title": "Nested Post",
                    "comments": {
                        "create": [{
                            "__id": "c100",
                            "content": "Nested Comment"
                        }]
                    }
                }]
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(setup_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Test A: Depth Validations
    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Nested User" },
        "data": {
            "posts": {
                "updateMany": {
                    "where": { "title": "Nested Post" },
                    "data": {
                        "comments": {
                            "updateMany": {
                                "where": { "content": "Nested Comment" },
                                "data": { "content": "Updated Deep Comment" }
                            }
                        }
                    }
                }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);
    
    let conn = pool.get().await.unwrap();
    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM Comment WHERE content = 'Updated Deep Comment'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 1);

    // Test B: Insert ... Select
    let create_payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Nested User" },
        "data": {
            "posts": {
                "create": [{
                    "__id": "p101",
                    "title": "Bulk Created Post"
                }]
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(create_payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::OK);

    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM Post WHERE title = 'Bulk Created Post'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 1);

    // Test C: Semantic Rejection
    let connect_payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Nested User" },
        "data": {
            "posts": {
                "connect": { "__id": "p100" }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(connect_payload.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("Semantics Error: Cannot 'connect' a child to multiple parents"));
}
