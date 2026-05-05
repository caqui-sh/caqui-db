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
            auditLogs: AuditLog[]
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
        model AuditLog {
            action: String
            userId: String?
            user: User?
            @@id(uuid)
        }
        
        base Content {
            activities: Activity[]
        }
        
        model Activity {
            metadata: String
            contentId: String?
            content: Content?
            @@id(uuid)
        }
        
        model Article extends Content {
            title: String
            @@id(uuid)
        }
        
        model Video extends Content {
            url: String
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
async fn test_sibling_bulk_mutations_and_cross_pollination() {
    let (app, pool, _dir) = setup_app().await;

    // Seed data
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name, email) VALUES ('u1', 'TargetUser', 't@t.com')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, authorId) VALUES ('p1', 'Post1', 'u1')", []).unwrap();
        db.execute("INSERT INTO Comment (__id, content, postId) VALUES ('c1', 'Comment1', 'p1')", []).unwrap();
        db.execute("INSERT INTO AuditLog (__id, action, userId) VALUES ('a1', 'TestAudit', 'u1')", []).unwrap();
    }).await.unwrap();

    // Test A & D: Sibling Bulk Mutations & Cross-Pollination
    // updateMany on User -> triggers updateMany on Post AND deleteMany on AuditLog AND create on AuditLog
    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "TargetUser" },
        "data": {
            "posts": {
                "updateMany": {
                    "where": { "title": "Post1" },
                    "data": { "title": "Post1_Updated" }
                }
            },
            "auditLogs": {
                "deleteMany": { "where": { "action": "TestAudit" } },
                "create": [{
                    "__id": "a2",
                    "action": "NewAudit"
                }]
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    let st = res.status(); let bb = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap(); let bs = String::from_utf8_lossy(&bb); println!("Status: {}, Body: {}", st, bs); assert_eq!(st, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    
    // Verify sibling UpdateMany worked correctly
    let post_title: String = conn.interact(|db| {
        db.query_row("SELECT title FROM Post WHERE __id = 'p1'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(post_title, "Post1_Updated");

    // Verify sibling DeleteMany worked correctly
    let audit_count: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM AuditLog WHERE action = 'TestAudit'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(audit_count, 0);

    // Verify sibling Create (Cross-Pollination) worked correctly
    let audit_count_new: i64 = conn.interact(|db| {
        db.query_row("SELECT COUNT(*) FROM AuditLog WHERE action = 'NewAudit' AND userId = 'u1'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(audit_count_new, 1);
}

#[tokio::test]
async fn test_intermediate_zero_match_safety() {
    let (app, pool, _dir) = setup_app().await;

    // Seed data
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name, email) VALUES ('u2', 'ZeroMatchUser', 'z@z.com')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, authorId) VALUES ('p2', 'Post2', 'u2')", []).unwrap();
        db.execute("INSERT INTO Comment (__id, content, postId) VALUES ('c2', 'Comment2', 'p2')", []).unwrap();
    }).await.unwrap();

    // Test B: Intermediate Zero-Match Safety
    let payload = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "ZeroMatchUser" },
        "data": {
            "posts": {
                "updateMany": {
                    "where": { "title": "NonExistentPost" }, // Will match 0 rows
                    "data": {
                        "comments": {
                            "updateMany": {
                                "where": { "content": "Comment2" },
                                "data": { "content": "GhostComment" }
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
    
    let st = res.status(); let bb = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap(); let bs = String::from_utf8_lossy(&bb); println!("Status: {}, Body: {}", st, bs); assert_eq!(st, StatusCode::OK); // Should execute safely without QueryReturnedNoRows panic

    // Verify comment was untouched
    let conn = pool.get().await.unwrap();
    let comment_content: String = conn.interact(|db| {
        db.query_row("SELECT content FROM Comment WHERE __id = 'c2'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(comment_content, "Comment2");
}

#[tokio::test]
async fn test_missing_semantic_rejections() {
    let (app, _pool, _dir) = setup_app().await;

    // Test C: Attempt to `update` inside a bulk `updateMany`
    let payload_update = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Any" },
        "data": {
            "posts": {
                "update": {
                    "where": { "__id": "p1" },
                    "data": { "title": "Invalid" }
                }
            }
        }
    });

    let res_update = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload_update.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res_update.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(res_update.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    println!("Update Error Body: {}", body_str); assert!(body_str.contains("Semantics Error: Cannot execute singular 'update' nested under a bulk operation"));

    // Test C: Attempt to `set` inside a bulk `updateMany`
    let payload_set = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Any" },
        "data": {
            "posts": {
                "set": []
            }
        }
    });

    let res_set = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload_set.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res_set.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(res_set.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("Semantics Error: Cannot 'set' a relation to multiple distinct parents in a bulk update"));

    // Test C: Attempt to `upsert` inside a bulk `updateMany`
    let payload_upsert = serde_json::json!({
        "model": "User",
        "action": "updateMany",
        "where": { "name": "Any" },
        "data": {
            "posts": {
                "upsert": {
                    "create": { "title": "New" },
                    "update": { "data": { "title": "Invalid" } }
                }
            }
        }
    });

    let res_upsert = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload_upsert.to_string())).unwrap()
    ).await.unwrap();
    
    assert_eq!(res_upsert.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(res_upsert.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("Semantics Error: Cannot 'upsert' a child to multiple parents in a bulk update"));
}

#[tokio::test]
async fn test_polymorphic_bulk_propagation() {
    let (app, pool, _dir) = setup_app().await;

    // Seed Data
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('art1', 'My Article')", []).unwrap();
        // Since Activity relates to Content (which Article extends), we link contentId to the Article's ID.
        // Wait, what is the exact column name? We will check.
        db.execute("INSERT INTO Activity (__id, metadata, contentId, parent_type) VALUES ('act1', 'Reading', 'art1', 'Article')", []).unwrap_or_else(|_| {
            db.execute("INSERT INTO Activity (__id, metadata, contentId) VALUES ('act1', 'Reading', 'art1')", []).unwrap();
            0
        });
    }).await.unwrap();

    // Test E: Polymorphic Bulk Propagation
    // updateMany on Content (virtual) -> updates Article -> updates Activity
    let payload = serde_json::json!({
        "model": "Article",
        "action": "updateMany",
        "where": { "title": "My Article" },
        "data": {
            "activities": {
                "updateMany": {
                    "where": { "metadata": "Reading" },
                    "data": { "metadata": "Finished Reading" }
                }
            }
        }
    });

    let res = app.clone().oneshot(
        Request::builder().method(http::Method::POST).uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string())).unwrap()
    ).await.unwrap();
    
    let st = res.status(); let bb = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap(); let bs = String::from_utf8_lossy(&bb); println!("Status: {}, Body: {}", st, bs); assert_eq!(st, StatusCode::OK);

    // Verify polymorphic subquery propagated effectively
    let conn = pool.get().await.unwrap();
    let activity_metadata: String = conn.interact(|db| {
        db.query_row("SELECT metadata FROM Activity WHERE __id = 'act1'", [], |row| row.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(activity_metadata, "Finished Reading");
}
