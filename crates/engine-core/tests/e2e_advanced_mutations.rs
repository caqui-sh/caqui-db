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
            user: User 
            @@id(uuid)
        }
        model Post {
            title: String
            authorId: String?
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
async fn test_relational_set_delete_dropped() {
    let (app, pool, _dir) = setup_app().await;

    // 1. Seed User and 2 Posts
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": {
                    "email": "u2@test.com",
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

    // 2. Set posts to ONLY p3 (deletes p1, p2)
    let set_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "u2@test.com" },
        "data": {
            "posts": { 
                "set": [{ "__id": p3_id }],
                "deleteDropped": true 
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(set_payload.to_string())).unwrap()
    ).await.unwrap();
    
    if res.status() != StatusCode::OK {
        let body = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
        panic!("Set operation with deleteDropped failed: {}", String::from_utf8_lossy(&body));
    }
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let connected_count: i64 = db.query_row("SELECT count(*) FROM Post WHERE authorId IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(connected_count, 1, "Only p3 should be connected");
        
        let total_count: i64 = db.query_row("SELECT count(*) FROM Post", [], |r| r.get(0)).unwrap();
        assert_eq!(total_count, 1, "p1 and p2 should be DELETED, leaving only p3");

        let p3_author: String = db.query_row("SELECT authorId FROM Post WHERE title = 'p3'", [], |r| r.get(0)).unwrap();
        assert!(!p3_author.is_empty());
    }).await.unwrap();
}

#[tokio::test]
async fn test_relational_set_complex_filter_and_empty_set() {
    let (app, pool, _dir) = setup_app().await;

    // Seed User and 3 Posts
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": {
                    "email": "complex@test.com",
                    "posts": { "create": [{ "title": "Post A" }, { "title": "Post B" }, { "title": "Post C" }] }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 1. Complex Filter set: retain only "Post B", dropping A and C
    let set_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "complex@test.com" },
        "data": {
            "posts": { 
                "set": [{ "title": "Post B" }],
                "deleteDropped": true 
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(set_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let total_count: i64 = db.query_row("SELECT count(*) FROM Post", [], |r| r.get(0)).unwrap();
        assert_eq!(total_count, 1, "Post A and Post C should be DELETED");

        let remaining_title: String = db.query_row("SELECT title FROM Post", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining_title, "Post B");
    }).await.unwrap();

    // 2. Empty set: []
    let empty_set_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "complex@test.com" },
        "data": {
            "posts": { 
                "set": [],
                "deleteDropped": true 
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(empty_set_payload.to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    conn.interact(|db| {
        let total_count: i64 = db.query_row("SELECT count(*) FROM Post", [], |r| r.get(0)).unwrap();
        assert_eq!(total_count, 0, "Post B should be DELETED, leaving no posts");
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_relation_delete_dropped() {
    let (app, pool, _dir) = setup_app().await;

    // Create User 1 with Profile A
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": {
                    "email": "user1@test.com",
                    "profile": { "create": { "bio": "Profile A" } }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Create User 2 with Profile B
    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "action": "create",
                "model": "User",
                "data": {
                    "email": "user2@test.com",
                    "profile": { "create": { "bio": "Profile B" } }
                }
            }).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let conn = pool.get().await.unwrap();
    let profile_b_id: String = conn.interact(|db| db.query_row("SELECT __id FROM Profile WHERE bio = 'Profile B'", [], |r| r.get(0))).await.unwrap().unwrap();

    // Update User 1 to SET profile to Profile B, dropping Profile A
    let set_payload = serde_json::json!({
        "action": "update",
        "model": "User",
        "where": { "email": "user1@test.com" },
        "data": {
            "profile": { 
                "set": { "__id": profile_b_id },
                "deleteDropped": true 
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(set_payload.to_string())).unwrap()
    ).await.unwrap();
    
    if res.status() != StatusCode::OK {
        let body = axum::body::to_bytes(res.into_body(), 10000).await.unwrap();
        panic!("Singular set operation failed: {}", String::from_utf8_lossy(&body));
    }
    assert_eq!(res.status(), StatusCode::OK);

    // Verify Profile A was physically deleted, and Profile B exists and points to User 1
    conn.interact(|db| {
        let total_count: i64 = db.query_row("SELECT count(*) FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(total_count, 1, "Profile A should be deleted");

        let bio: String = db.query_row("SELECT bio FROM Profile", [], |r| r.get(0)).unwrap();
        assert_eq!(bio, "Profile B", "Profile B should be the only remaining profile");

        let user1_id: String = db.query_row("SELECT __id FROM User WHERE email = 'user1@test.com'", [], |r| r.get(0)).unwrap();
        let profile_user_id: String = db.query_row("SELECT userId FROM Profile WHERE bio = 'Profile B'", [], |r| r.get(0)).unwrap();
        assert_eq!(user1_id, profile_user_id, "Profile B should be connected to User 1");
    }).await.unwrap();
}

