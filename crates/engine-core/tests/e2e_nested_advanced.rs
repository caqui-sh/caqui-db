use tempfile::tempdir;
use std::process::Command;
use std::fs;
use std::env;
use std::sync::Arc;
use axum::{body::Body, http::{self, Request, StatusCode}};
use tower::util::ServiceExt;
use serde_json::{Value, json};

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

// --------------------------------------------------------------------------------
// UNHAPPY PATHS (EXPLICIT BOUNDARY REJECTIONS)
// --------------------------------------------------------------------------------

#[tokio::test]
async fn test_reject_connect_set_in_updatemany() {
    let schema = r#"
        model Config { settings: String @@id(uuid) }
        model User { 
            status: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    
    conn.interact(|db| {
        db.execute("INSERT INTO Config (__id, settings) VALUES ('c1', 'old')", []).unwrap();
        db.execute("INSERT INTO User (__id, status) VALUES ('u1', 'active')", []).unwrap();
    }).await.unwrap();

    // CONNECT
    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "config": { "connect": { "__id": "c1" } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error"));

    // SET
    let payload_set = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "config": { "set": [{ "__id": "c1" }] } }
    });
    let (status_set, res_set) = post_query(&app, payload_set).await;
    assert_eq!(status_set, StatusCode::BAD_REQUEST);
    assert!(res_set["error"].as_str().unwrap().contains("Semantics Error"));
}

#[tokio::test]
async fn test_allow_polymorphic_update_in_updatemany() {
    let schema = r#"
        base Content {}
        model Article extends Content { title: String @@id(uuid) }
        model User {
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('a1', 'Hello')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, favorite_id, favorite_type) VALUES ('u1', 'active', 'a1', 'Article')", []).unwrap();
    }).await.unwrap();

    // POLYMORPHIC UPDATE
    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "favorite": { "update": { "where": {"__kind": "Article"}, "data": { "title": "New" } } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed: {:?}", res);
}

// --------------------------------------------------------------------------------
// NESTED DELETE UNDER UPDATEMANY
// --------------------------------------------------------------------------------

#[tokio::test]
async fn test_nested_delete_under_updatemany_forward() {
    let schema = r#"
        model Config { settings: String @@id(uuid) }
        model User { 
            status: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Config (__id, settings) VALUES ('c1', 'cfg1'), ('c2', 'cfg2'), ('c3', 'cfg3')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, configId) VALUES ('u1', 'active', 'c1')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, configId) VALUES ('u2', 'active', 'c2')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, configId) VALUES ('u3', 'inactive', 'c3')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "config": { "delete": true } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);

    conn.interact(|db| {
        let count: i64 = db.query_row("SELECT count(*) FROM Config", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "Only c3 should remain");
        
        let c1_count: i64 = db.query_row("SELECT count(*) FROM User WHERE configId IS NOT NULL AND status = 'active'", [], |r| r.get(0)).unwrap();
        assert_eq!(c1_count, 0, "FK should be nullified (if supported) or Config deleted");
    }).await.unwrap();
}

#[tokio::test]
async fn test_nested_delete_under_updatemany_polymorphic() {
    let schema = r#"
        base Content {}
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        model User {
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('a1', 'Art1'), ('a2', 'Art2')", []).unwrap();
        db.execute("INSERT INTO Video (__id, duration) VALUES ('v1', 120)", []).unwrap();
        db.execute("INSERT INTO User (__id, status, favorite_id, favorite_type) VALUES ('u1', 'active', 'a1', 'Article')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, favorite_id, favorite_type) VALUES ('u2', 'active', 'v1', 'Video')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, favorite_id, favorite_type) VALUES ('u3', 'inactive', 'a2', 'Article')", []).unwrap();
    }).await.unwrap();

    // Since it's polymorphic, the IR handles the delete inside `updateMany`. But wait, `updateMany` for polymorphic delete might not be fully supported based on the error "Unsupported deferred action". Let's verify.
    // Actually, earlier we saw that `DeferredAction::Delete` is supported for singular polymorphic inside `process_deferred_children`.
    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "favorite": { "delete": { "where": { "__kind": "Article" } } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);

    conn.interact(|db| {
        let art_count: i64 = db.query_row("SELECT count(*) FROM Article", [], |r| r.get(0)).unwrap();
        assert_eq!(art_count, 1, "Only a2 should remain, a1 should be deleted");
        
        let vid_count: i64 = db.query_row("SELECT count(*) FROM Video", [], |r| r.get(0)).unwrap();
        assert_eq!(vid_count, 1, "Video v1 should NOT be deleted");
    }).await.unwrap();
}

// --------------------------------------------------------------------------------
// NESTED UPDATE UNDER UPDATEMANY
// --------------------------------------------------------------------------------

#[tokio::test]
async fn test_nested_update_under_updatemany_forward() {
    let schema = r#"
        model Config { settings: String @@id(uuid) }
        model User { 
            status: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Config (__id, settings) VALUES ('c1', 'old')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, configId) VALUES ('u1', 'active', 'c1')", []).unwrap();
        db.execute("INSERT INTO User (__id, status, configId) VALUES ('u2', 'active', 'c1')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": { "config": { "update": { "where": {}, "data": { "settings": "updated" } } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);

    conn.interact(|db| {
        let set: String = db.query_row("SELECT settings FROM Config WHERE __id = 'c1'", [], |r| r.get(0)).unwrap();
        assert_eq!(set, "updated");
    }).await.unwrap();
}

// --------------------------------------------------------------------------------
// SINGULAR CONSTRAINT REGRESSIONS
// --------------------------------------------------------------------------------

#[tokio::test]
async fn test_singular_regression_forward_create() {
    let schema = r#"
        model Config { settings: String @@id(uuid) }
        model User { 
            status: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    let payload = json!({
        "model": "User", "action": "create",
        "data": { "status": "active", "config": { "create": { "settings": "single_cfg" } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);

    let created_user = res["data"].clone();
    assert!(created_user["__id"].is_string());
    // Ensure configId is set (we can't check directly if not selected, but execution should succeed).
}

#[tokio::test]
async fn test_singular_regression_polymorphic_create() {
    let schema = r#"
        base Content {}
        model Article extends Content { title: String @@id(uuid) }
        model User {
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    let payload = json!({
        "model": "User", "action": "create",
        "data": { "status": "active", "favorite": { "create": { "__kind": "Article", "title": "single_art" } } }
    });
    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);
    assert!(res["data"]["__id"].is_string());
}

// --------------------------------------------------------------------------------
// COMPLEX CHAIN TOPOLOGIES (DEEP NESTING)
// --------------------------------------------------------------------------------

#[tokio::test]
async fn test_deep_nesting_bulk_singular_singular() {
    let schema = r#"
        model Metadata { key: String @@id(uuid) }
        model Config { 
            settings: String
            metaId: String?
            meta: Metadata? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
        model User { 
            status: String 
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid) 
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, status) VALUES ('u1', 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, status) VALUES ('u2', 'active')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User", "action": "updateMany", "where": { "status": "active" },
        "data": {
            "config": {
                "create": { 
                    "settings": "shared_config",
                    "meta": {
                        "create": { "key": "deep_meta" }
                    }
                }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Res: {:?}", res);

    conn.interact(|db| {
        let meta_count: i64 = db.query_row("SELECT count(*) FROM Metadata", [], |r| r.get(0)).unwrap();
        assert_eq!(meta_count, 1, "Exactly one deep Metadata record created");
        
        let cfg_count: i64 = db.query_row("SELECT count(*) FROM Config", [], |r| r.get(0)).unwrap();
        assert_eq!(cfg_count, 1, "Exactly one Config record created");
        
        let meta_key: String = db.query_row("SELECT key FROM Metadata", [], |r| r.get(0)).unwrap();
        assert_eq!(meta_key, "deep_meta");
    }).await.unwrap();
}
