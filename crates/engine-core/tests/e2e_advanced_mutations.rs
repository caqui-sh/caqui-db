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
            tags: String[]
            profile: Profile?
            posts: Post[]
            @@id(uuid)
        }
        model Profile {
            bio: String
            userId: String @unique
            user: User @relation(fields: [userId], references: [__id])
            @@id(uuid)
        }
        model Post {
            title: String
            authorId: String?
            author: User? @relation(fields: [authorId], references: [__id])
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
async fn test_scalar_array_mutations() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Create with initial tags
    let create_payload = serde_json::json!({
        "action": "create",
        "model": "User",
        "data": { "email": "a@test.com", "tags": ["db"] }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(create_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. Push tag
    let push_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "a@test.com" },
        "data": { "tags": { "push": "rust" } }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(push_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM User WHERE email = 'a@test.com'", [], |r| r.get(0)).unwrap();
        let tags_val: Value = serde_json::from_str(&tags).unwrap();
        assert_eq!(tags_val, serde_json::json!(["db", "rust"]));
    }).await.unwrap();

    // 3. Overwrite tags
    let replace_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "a@test.com" },
        "data": { "tags": ["new"] }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(replace_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let tags: String = db.query_row("SELECT tags FROM User WHERE email = 'a@test.com'", [], |r| r.get(0)).unwrap();
        let tags_val: Value = serde_json::from_str(&tags).unwrap();
        assert_eq!(tags_val, serde_json::json!(["new"]));
    }).await.unwrap();
}

#[tokio::test]
async fn test_nested_update_and_delete() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Seed data
    let seed_payload = serde_json::json!({
        "action": "create",
        "model": "User",
        "data": {
            "email": "u1@test.com",
            "profile": { "create": { "bio": "original" } },
            "posts": { "create": [{ "title": "p1" }, { "title": "p2" }] }
        }
    });
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(seed_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. Nested Update Profile AND Delete Post
    let nested_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "u1@test.com" },
        "data": {
            "profile": { "update": { "where": {}, "data": { "bio": "changed" } } },
            "posts": { "delete": [{ "title": "p1" }] }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(nested_payload.to_string())).unwrap()
    ).await.unwrap();
    
    if res.status() != StatusCode::OK {
        let body = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
        panic!("Nested update failed: {}", String::from_utf8_lossy(&body));
    }
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let bio: String = db.query_row("SELECT bio FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(bio, "changed");

        let post_count: i64 = db.query_row("SELECT count(*) FROM Post", [], |r| r.get(0)).unwrap();
        assert_eq!(post_count, 1, "One post should have been deleted");
    }).await.unwrap();
}

#[tokio::test]
async fn test_relational_set_operation() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Seed User and 2 Posts
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": {
                    "email": "u1@test.com",
                    "posts": { "create": [{ "title": "p1" }, { "title": "p2" }] }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Seed p3 orphaned
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create", "model": "Post", "data": { "title": "p3" }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    
    let conn = pool.get().await.unwrap();
    let p3_id: String = conn.interact(|db| db.query_row("SELECT __id FROM Post WHERE title = 'p3'", [], |r| r.get(0))).await.unwrap().unwrap();

    // 2. Set posts to ONLY p3 (disconnects p1, p2)
    let set_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "u1@test.com" },
        "data": {
            "posts": { "set": [{ "__id": p3_id }] }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(set_payload.to_string())).unwrap()
    ).await.unwrap();
    
    if res.status() != StatusCode::OK {
        let body = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
        panic!("Set operation failed: {}", String::from_utf8_lossy(&body));
    }
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let connected_count: i64 = db.query_row("SELECT count(*) FROM Post WHERE authorId IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(connected_count, 1, "Only p3 should be connected");
        
        let p3_author: String = db.query_row("SELECT authorId FROM Post WHERE title = 'p3'", [], |r| r.get(0)).unwrap();
        assert!(!p3_author.is_empty());
    }).await.unwrap();
}

#[tokio::test]
async fn test_nested_upsert_logic() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Seed User
    let _ = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create", "model": "User", "data": { "email": "u1@test.com" }
            }).to_string())).unwrap()
    ).await.unwrap();

    // 2. Nested Upsert (Create Path)
    let upsert_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "u1@test.com" },
        "data": {
            "profile": {
                "upsert": {
                    "create": { "bio": "created via upsert" },
                    "update": { "bio": "should not happen" }
                }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(upsert_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let bio: String = db.query_row("SELECT bio FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(bio, "created via upsert");
    }).await.unwrap();

    // 3. Nested Upsert (Update Path)
    let update_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "u1@test.com" },
        "data": {
            "profile": {
                "upsert": {
                    "create": { "bio": "should not happen" },
                    "update": { "bio": "updated via upsert" }
                }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(update_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let bio: String = db.query_row("SELECT bio FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(bio, "updated via upsert");
    }).await.unwrap();
}
