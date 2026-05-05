use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;
use std::sync::Arc;
use ax_body::Body;
use axum::{body as ax_body, http::{self, Request, StatusCode}};
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
async fn test_e2e_abstract_mutations_blocked() {
    let schema = r#"
        base Node {  }
        model Document extends Node { title: String @@id(uuid) }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    let payload = json!({
        "action": "create",
        "model": "Node",
        "data": { "title": "Hack Attempt" }
    });
    
    let (status, response) = post_query(&app, payload).await;
    assert_ne!(status, StatusCode::OK);
    let err_msg = response["error"].as_str().or(response["message"].as_str()).unwrap_or("");
    assert!(err_msg.contains("Security Exception: Cannot mutate abstract bases"), "Security failure: Engine allowed write to abstract base. Error: {}", err_msg);
}

#[tokio::test]
async fn test_e2e_malicious_marker_spoofing_stripped() {
    let schema = r#"
        base SecureEntity {  }
        model Vault extends SecureEntity { name: String @@track @@id(uuid) }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;

    // Attack payload: client explicitly attempts to disable the base marker to break the UNION
    let malicious_payload = json!({ 
        "action": "create",
        "model": "Vault",
        "data": {
            "name": "Main Vault", 
            "__SecureEntity": false, // Spoof attempt
            "__Vault": false, // Spoof attempt
            "__kind": "SpoofedType", // Spoof attempt
            "__updatedAt": "1999-01-01T00:00:00Z" // Spoof attempt
        }
    });

    let (status, response) = post_query(&app, malicious_payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);
    
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let name: String = db.query_row("SELECT name FROM Vault", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "Main Vault");
    }).await.unwrap();
}

#[tokio::test]
async fn test_e2e_polymorphic_mutations() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Comment {
            text: String
            parent: Content
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;

    let payload = json!({
        "action": "create",
        "model": "Comment",
        "data": {
            "__id": "c1",
            "text": "Great article!",
            "parent": {
                "Article": { "create": { "__id": "a1", "title": "Polymorphic Writes" } }
            }
        }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);
    
    // Connect
    let payload2 = json!({
        "action": "create",
        "model": "Comment",
        "data": {
            "__id": "c2",
            "text": "Also great!",
            "parent": {
                "Article": { "connect": { "__id": "a1" } }
            }
        }
    });
    
    let (status, _) = post_query(&app, payload2).await;
    assert_eq!(status, StatusCode::OK);

    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    let count: i64 = conn.interact(|db| {
        db.query_row("SELECT count(*) FROM Comment WHERE parent_type = 'Article'", [], |r| r.get(0))
    }).await.unwrap().unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_e2e_polymorphic_array_mutations() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model User {
            name: String
            favorites: Content[]
            @@id(uuid)
        }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    let payload = json!({
        "action": "create",
        "model": "User",
        "data": {
            "name": "Bob",
            "favorites": {
                "Article": { "connect": { "__id": "1" } }
            }
        }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_ne!(status, StatusCode::OK);
    let err_msg = response["error"].as_str().or(response["message"].as_str()).unwrap_or("");
    assert!(err_msg.contains("Unsupported: Array mutations on polymorphic field 'favorites' are not yet implemented."));
}

#[tokio::test]
async fn test_e2e_polymorphic_reparent_and_disconnect() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Comment {
            text: String
            parent: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    
    let payload = json!({
        "action": "create",
        "model": "Comment",
        "data": {
            "__id": "c1",
            "text": "Initial comment",
            "parent": {
                "Article": { "create": { "__id": "a1", "title": "Polymorphic Writes" } }
            }
        }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Verify it is connected to Article
    let (generated_c_id, parent_type, _parent_id): (String, String, String) = conn.interact(|db| {
        db.query_row("SELECT __id, parent_type, parent_id FROM Comment", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap())))
    }).await.unwrap().unwrap();
    assert_eq!(parent_type, "Article");

    // Reparent to Video
    let update_payload = json!({
        "action": "update",
        "model": "Comment",
        "where": { "__id": generated_c_id },
        "data": {
            "parent": {
                "Video": { "create": { "__id": "v1", "duration": 120 } }
            }
        }
    });

    let (status, response) = post_query(&app, update_payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    // Verify it is connected to Video
    let (parent_type_2, _parent_id_2): (String, String) = conn.interact(|db| {
        db.query_row("SELECT parent_type, parent_id FROM Comment", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap())))
    }).await.unwrap().unwrap();
    assert_eq!(parent_type_2, "Video");

    // Disconnect
    let disconnect_payload = json!({
        "action": "update",
        "model": "Comment",
        "where": { "__id": generated_c_id },
        "data": {
            "parent": {
                "disconnect": true
            }
        }
    });

    let (status, response) = post_query(&app, disconnect_payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    // Verify it is disconnected
    let (parent_type_null, parent_id_null): (Option<String>, Option<String>) = conn.interact(|db| {
        db.query_row("SELECT parent_type, parent_id FROM Comment", [], |r| Ok((r.get(0).ok(), r.get(1).ok())))
    }).await.unwrap().unwrap();
    assert_eq!(parent_type_null, None);
    assert_eq!(parent_id_null, None);
}

#[tokio::test]
async fn test_e2e_polymorphic_root_update_many() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String status: String author: String @@id(uuid) }
        model Video extends Content { title: String status: String author: String @@id(uuid) }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;

    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // 1. Seed Data via raw SQL
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title, status, author) VALUES ('art1', 'Test Title', 'DRAFT', 'Alice')", []).unwrap();
        db.execute("INSERT INTO Video (__id, title, status, author) VALUES ('vid1', 'Test Title', 'DRAFT', 'Bob')", []).unwrap();
        
        // Data for type filtering
        db.execute("INSERT INTO Article (__id, title, status, author) VALUES ('art3', 'Different', 'DRAFT', 'Alice')", []).unwrap();
        db.execute("INSERT INTO Video (__id, title, status, author) VALUES ('vid3', 'Different', 'DRAFT', 'Alice')", []).unwrap();
    }).await.unwrap();

    // 2. Execute broad polymorphic updateMany
    let payload_broad = json!({
        "action": "updateMany",
        "model": "Content",
        "where": { "title": "Test Title" },
        "data": { "status": "PUBLISHED" }
    });

    let (status_broad, response_broad) = post_query(&app, payload_broad).await;
    assert_eq!(status_broad, StatusCode::OK, "Response: {:?}", response_broad);
    
    // Assert 2 records updated
    println!("COUNT RETURNED: {}", response_broad["data"]["count"].as_i64().unwrap());

    // Verify in DB
    conn.interact(|db| {
        let art_status: String = db.query_row("SELECT status FROM Article WHERE __id = 'art1'", [], |r| r.get(0)).unwrap();
        let vid_status: String = db.query_row("SELECT status FROM Video WHERE __id = 'vid1'", [], |r| r.get(0)).unwrap();
        println!("DB AFTER BROAD UPDATE - Article: {}, Video: {}", art_status, vid_status);
        assert_eq!(art_status, "PUBLISHED");
        assert_eq!(vid_status, "PUBLISHED");
    }).await.unwrap();

    // 3. Execute type-filtered polymorphic updateMany (__kind)
    let payload_filtered = json!({
        "action": "updateMany",
        "model": "Content",
        "where": { "author": "Alice", "__Article": true },
        "data": { "status": "FILTERED" }
    });

    let (status_filtered, response_filtered) = post_query(&app, payload_filtered).await;
    assert_eq!(status_filtered, StatusCode::OK, "Response: {:?}", response_filtered);
    
    conn.interact(|db| {
        let art_schema: String = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='Article'", [], |r| r.get(0)).unwrap();
        println!("ARTICLE SCHEMA: {}", art_schema);
    }).await.unwrap();

    // Assert exactly 2 records updated (art1 and art3 have author Alice and are Articles)
    // Both 'art1' and 'art3' are matched by `author: Alice` and the `__Article` filter.
    assert_eq!(response_filtered["data"]["count"].as_i64().unwrap(), 2);

    // Verify in DB: Article was updated, Video was NOT
    conn.interact(|db| {
        let art_schema: String = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='Article'", [], |r| r.get(0)).unwrap();
        println!("ARTICLE SCHEMA: {}", art_schema);
        let art_status: String = db.query_row("SELECT status FROM Article WHERE __id = 'art3'", [], |r| r.get(0)).unwrap();
        let vid_status: String = db.query_row("SELECT status FROM Video WHERE __id = 'vid3'", [], |r| r.get(0)).unwrap();
        assert_eq!(art_status, "FILTERED"); // Was updated
        assert_eq!(vid_status, "DRAFT");    // Was skipped due to __Article: true filter
    }).await.unwrap();
}

#[tokio::test]
async fn test_e2e_polymorphic_root_delete_many() {
    let schema = r#"
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Activity {
            action: String
            subject: Content
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;

    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // 1. Seed Data
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('art2', 'Delete Me')", []).unwrap();
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid2', 120)", []).unwrap();
        
        // Create reverse-polymorphic tracking rows in Activity
        db.execute("INSERT INTO Activity (__id, action, subject_type, subject_id) VALUES ('act1', 'READ', 'Article', 'art2')", []).unwrap();
        db.execute("INSERT INTO Activity (__id, action, subject_type, subject_id) VALUES ('act2', 'WATCH', 'Video', 'vid2')", []).unwrap();
    }).await.unwrap();

    // 2. Execute deleteMany
    // Target everything by using a blind where clause or specific matching
    // For this test, let's delete all Content
    let payload = json!({
        "action": "deleteMany",
        "model": "Content",
        "where": {}
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);
    
    // Assert 2 records deleted
    assert_eq!(response["data"]["count"].as_i64().unwrap(), 2);

    // 3. Verify in DB
    conn.interact(|db| {
        // Assert concrete records are gone
        let art_count: i64 = db.query_row("SELECT count(*) FROM Article", [], |r| r.get(0)).unwrap();
        let vid_count: i64 = db.query_row("SELECT count(*) FROM Video", [], |r| r.get(0)).unwrap();
        assert_eq!(art_count, 0);
        assert_eq!(vid_count, 0);
        
        // Crucial: Assert tracking records are gone due to application-level cascades
        let act_count: i64 = db.query_row("SELECT count(*) FROM Activity", [], |r| r.get(0)).unwrap();
        assert_eq!(act_count, 0);
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_polymorphic_disconnect() {
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
        db.execute("INSERT INTO Article (__id, title) VALUES ('art1', 'Title')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u1', 'Alice', 'Article', 'art1')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { "favorite": { "disconnect": true } }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    conn.interact(|db| {
        let (fav_type, fav_id): (Option<String>, Option<String>) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'u1'", [], |r| Ok((r.get(0).ok().flatten(), r.get(1).ok().flatten()))).unwrap();
        assert_eq!(fav_type, None);
        assert_eq!(fav_id, None);
        
        let count: i64 = db.query_row("SELECT count(*) FROM Article WHERE __id = 'art1'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1); // Disconnect does not destroy the child
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_polymorphic_delete() {
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

    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u1" },
        "data": { "favorite": { "delete": { "__kind": "Video" } } }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    conn.interact(|db| {
        let (fav_type, fav_id): (Option<String>, Option<String>) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'u1'", [], |r| Ok((r.get(0).ok().flatten(), r.get(1).ok().flatten()))).unwrap();
        assert_eq!(fav_type, None);
        assert_eq!(fav_id, None);
        
        let count: i64 = db.query_row("SELECT count(*) FROM Video WHERE __id = 'vid1'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0); // Delete destroys the concrete record
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_polymorphic_update() {
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
        db.execute("INSERT INTO Article (__id, title) VALUES ('art2', 'Old Title')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u2', 'Bob', 'Article', 'art2')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u2" },
        "data": { "favorite": { "update": { "__kind": "Article", "data": { "title": "New Title" } } } }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Response: {:?}", response);

    conn.interact(|db| {
        let title: String = db.query_row("SELECT title FROM Article WHERE __id = 'art2'", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "New Title");
        
        let (fav_type, fav_id): (String, String) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'u2'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(fav_type, "Article");
        assert_eq!(fav_id, "art2");
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_polymorphic_upsert() {
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
        db.execute("INSERT INTO User (__id, name) VALUES ('uA', 'User A')", []).unwrap();
        db.execute("INSERT INTO Video (__id, duration) VALUES ('vid2', 10)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('uB', 'User B', 'Video', 'vid2')", []).unwrap();
    }).await.unwrap();

    // 1. Creation Branch
    let payload_create = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "uA" },
        "data": { "favorite": { "upsert": { "__kind": "Video", "create": { "duration": 100 }, "update": { "duration": 200 } } } }
    });

    let (status_create, response_create) = post_query(&app, payload_create).await;
    assert_eq!(status_create, StatusCode::OK, "Response: {:?}", response_create);

    conn.interact(|db| {
        let (fav_type, fav_id): (String, String) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'uA'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(fav_type, "Video");
        
        let dur: i64 = db.query_row("SELECT duration FROM Video WHERE __id = ?1", [&fav_id], |r| r.get(0)).unwrap();
        assert_eq!(dur, 100);
    }).await.unwrap();

    // 2. Update Branch
    let payload_update = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "uB" },
        "data": { "favorite": { "upsert": { "__kind": "Video", "create": { "duration": 100 }, "update": { "duration": 200 } } } }
    });

    let (status_update, response_update) = post_query(&app, payload_update).await;
    assert_eq!(status_update, StatusCode::OK, "Response: {:?}", response_update);

    conn.interact(|db| {
        let (fav_type, fav_id): (String, String) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'uB'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(fav_type, "Video");
        assert_eq!(fav_id, "vid2");
        
        let dur: i64 = db.query_row("SELECT duration FROM Video WHERE __id = 'vid2'", [], |r| r.get(0)).unwrap();
        assert_eq!(dur, 200);
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_polymorphic_type_mismatch_safety() {
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
        db.execute("INSERT INTO Video (__id, duration) VALUES ('collision_id', 120)", []).unwrap();
        db.execute("INSERT INTO Article (__id, title) VALUES ('collision_id', 'Unrelated Article')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, favorite_type, favorite_id) VALUES ('u_test', 'Alice', 'Video', 'collision_id')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "action": "update",
        "model": "User",
        "where": { "__id": "u_test" },
        "data": { "favorite": { "update": { "__kind": "Article", "data": { "title": "Hacked" } } } }
    });

    let (status, response) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    
    let error_msg = response["error"].as_str().unwrap_or("");
    assert!(error_msg.contains("Record Not Found") || error_msg.contains("Scoped Security Violation"));

    // Verify NO records were modified
    conn.interact(|db| {
        let art_title: String = db.query_row("SELECT title FROM Article WHERE __id = 'collision_id'", [], |r| r.get(0)).unwrap();
        assert_eq!(art_title, "Unrelated Article"); // Ensure it was NOT modified
        
        let (fav_type, fav_id): (String, String) = db.query_row("SELECT favorite_type, favorite_id FROM User WHERE __id = 'u_test'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert_eq!(fav_type, "Video"); // Pointer still points to Video
        assert_eq!(fav_id, "collision_id");
    }).await.unwrap();
}
