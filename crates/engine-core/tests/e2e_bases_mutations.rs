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

    let (status, _) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK);
    
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
