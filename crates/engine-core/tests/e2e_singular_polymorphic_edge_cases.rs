use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;
use std::sync::Arc;
use axum::{body::Body, http::{self, Request, StatusCode}};
use tower::util::ServiceExt;
use serde_json::{json, Value};

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command {:?} failed!\nstdout: {}\nstderr: {}", cmd, stdout, stderr);
    }
    stdout
}

async fn setup_app(schema: &str) -> (axum::Router, tempfile::TempDir, String) {
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
    
    (app, dir, db_uri)
}

async fn post_query(app: &axum::Router, payload: Value) -> (StatusCode, Value) {
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
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
        json!({ "error": String::from_utf8_lossy(&body_bytes) })
    });
    (status, body_val)
}

#[tokio::test]
async fn test_reparenting_and_type_swapping() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Setup: User favors a Video
    conn.interact(|db| {
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid1', 120)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Video', 'vid1')", []).unwrap();
    }).await.unwrap();

    // Update User: Overwrite favorite with a NEW Article (re-parenting + type swap)
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { 
                    "create": { "title": "New Article" } 
                } 
            } 
        }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    conn.interact(|db| {
        let (fav_type, fav_id): (String, String) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'u1'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(fav_type, "Article");
        assert_ne!(fav_id, "vid1"); // Should point to a generated Article ID
        
        // Ensure the article was created
        let title: String = db.query_row("SELECT title FROM Article WHERE __id = ?", [fav_id], |r| r.get(0)).unwrap();
        assert_eq!(title, "New Article");
        
        // Old video should still exist unharmed
        let count: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'vid1'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }).await.unwrap();
}

#[tokio::test]
async fn test_multi_level_deep_nesting() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String tags: String[] @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Setup: User favors an Article
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title, tags) VALUES ('art1', 'Old Title', '[\"initial\"]')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Article', 'art1')", []).unwrap();
    }).await.unwrap();

    // Update User -> Update Article -> Multi-Item Push tags
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { 
                    "update": { 
                        "where": {},
                        "data": {
                            "title": "Nested Update",
                            "tags": { "push": ["deep"] }
                        }
                    } 
                } 
            } 
        }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    conn.interact(|db| {
        let (title, tags): (String, String) = db.query_row("SELECT title, tags FROM Article WHERE __id = 'art1'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(title, "Nested Update");
        assert_eq!(tags, "[\"initial\",\"deep\"]");
    }).await.unwrap();
}

#[tokio::test]
async fn test_payload_structure_validations_and_rejections() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    // 1. Multiple Target Wrappers
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { "disconnect": true },
                "Video": { "disconnect": true }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("requires exactly one target type"));

    // 2. Unsupported Batch Actions
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { "updateMany": { "where": {}, "data": { "title": "Nope" } } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Unsupported action for polymorphic field 'favorite'"));

    // 3. Malformed Action Blocks (missing where/data)
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { "update": { "title": "Missed Data Block" } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Expected 'where' object in 'update'"));
}

#[tokio::test]
async fn test_null_state_interactions() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Setup: User with completely NULL favorite
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name) VALUES ('u_null', 'Null Bob')", []).unwrap();
    }).await.unwrap();

    // 1. Disconnecting already disconnected relation
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u_null" },
        "data": { 
            "favorite": { 
                "Article": { "disconnect": true }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response); // Should succeed silently

    // 2. Updating a Null Relation (should fail securely due to zero rows affected)
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u_null" },
        "data": { 
            "favorite": { 
                "Article": { "update": { "where": {}, "data": { "title": "Ghost" } } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Record Not Found"));

    // 3. Deleting a Null Relation (should fail securely due to zero rows affected)
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u_null" },
        "data": { 
            "favorite": { 
                "Article": { "delete": { "where": {} } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Record Not Found"));
}

#[tokio::test]
async fn test_conditional_scoped_deletions() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid1', 120)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Video', 'vid1')", []).unwrap();
    }).await.unwrap();

    // Send a delete where the condition fails (duration = 999)
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Video": { "delete": { "where": { "duration": 999 } } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Record Not Found") || response["error"].as_str().unwrap().contains("Scoped Security Violation"));

    // Verify record was NOT deleted
    conn.interact(|db| {
        let count: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'vid1'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }).await.unwrap();
}

#[tokio::test]
async fn test_cross_type_deletion_rejections() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Video (__id, duration) VALUES ('shared_id', 120)", []).unwrap();
        db.execute("INSERT INTO Article (__id, title) VALUES ('shared_id', 'Secret')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Video', 'shared_id')", []).unwrap();
    }).await.unwrap();

    // Send a delete targeting the Article table, trying to spoof via the ID
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Article": { "delete": { "where": {} } }
            } 
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("Record Not Found") || response["error"].as_str().unwrap().contains("Scoped Security Violation"));

    // Verify neither record was deleted
    conn.interact(|db| {
        let count: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'shared_id'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let count_art: i64 = db.query_row("SELECT count(*) FROM Article WHERE __id = 'shared_id'", [], |r| r.get(0)).unwrap();
        assert_eq!(count_art, 1);
    }).await.unwrap();
}

#[tokio::test]
async fn test_dangling_pointer_read_safety() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid1', 120)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Video', 'vid1')", []).unwrap();
    }).await.unwrap();

    // Perform successful nested delete
    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { 
            "favorite": { 
                "Video": { "delete": { "where": {} } }
            } 
        }
    });
    let (status, _) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK);

    // Read the user, fetching the favorite. Should safely return null without panicking on FK missing.
    let payload = json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": { 
            "name": true,
            "favorite": {
                "Video": { "select": { "duration": true } }
            }
        }
    });
    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK);
    
    let rows = response["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let user = &rows[0];
    assert_eq!(user["name"], "Alice");
    assert!(user["favorite"].is_null()); // The crucial read safety assertion
}

#[tokio::test]
async fn test_nested_deletes_under_batch_operations() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid1', 120)", []).unwrap();
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid2', 300)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Video', 'vid1')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u2', 'Bob', 'Video', 'vid2')", []).unwrap();
    }).await.unwrap();

    // Target User u1 via batch updateMany, nesting a delete
    let payload = json!({
        "action": "updateMany",
        "model": "User",
        "where": { "name": "Alice" }, // Targets only u1
        "data": { 
            "favorite": { 
                "Video": { "delete": { "where": {} } }
            } 
        }
    });
    let (status, _) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK);

    // Verify vid1 is gone, vid2 is perfectly safe
    conn.interact(|db| {
        let count1: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'vid1'", [], |r| r.get(0)).unwrap();
        assert_eq!(count1, 0); // Deleted
        
        let count2: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'vid2'", [], |r| r.get(0)).unwrap();
        assert_eq!(count2, 1); // Unharmed
    }).await.unwrap();
}
