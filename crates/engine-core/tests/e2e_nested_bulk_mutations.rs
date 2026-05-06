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

#[tokio::test]
async fn test_update_many_nested_create_standard_forward() {
    let schema = r#"
        model Config {
            settings: String
            @@id(uuid)
        }
        
        model User {
            name: String
            status: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Setup: 2 Users
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u1', 'Alice', 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u2', 'Bob', 'active')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "config": {
                "create": { "settings": "global_config" }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    let count = res["data"]["count"].as_i64().unwrap();
    assert_eq!(count, 2);

    // verify only ONE config was created
    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Config").unwrap();
        let config_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(config_count, 1, "Expected exactly 1 Config to be created");
        
        let mut stmt = db.prepare("SELECT configId FROM User ORDER BY name").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let c1: String = rows.next().unwrap().unwrap().get(0).unwrap();
        let c2: String = rows.next().unwrap().unwrap().get(0).unwrap();
        assert_eq!(c1, c2, "Both users should share the same config reference");
    }).await.unwrap();
}

#[tokio::test]
async fn test_update_many_nested_create_singular_polymorphic() {
    let schema = r#"
        base Content { }
        model Article extends Content { title: String @@id(uuid) }
        
        model User {
            name: String
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    // Setup: 2 Users
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u1', 'Alice', 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u2', 'Bob', 'active')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "favorite": {
                "create": { "__kind": "Article", "title": "Shared Article" }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    let count = res["data"]["count"].as_i64().unwrap();
    assert_eq!(count, 2);

    // verify only ONE article was created
    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Article").unwrap();
        let article_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(article_count, 1, "Expected exactly 1 Article to be created");
        
        let mut stmt = db.prepare("SELECT favorite_id, favorite_type FROM User ORDER BY name").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut f_data = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            f_data.push((row.get::<_, String>(0).unwrap(), row.get::<_, String>(1).unwrap()));
        }
        
        assert_eq!(f_data[0].0, f_data[1].0, "Both users should share the same article reference");
        assert_eq!(f_data[0].1, "Article");
        assert_eq!(f_data[1].1, "Article");
    }).await.unwrap();
}

#[tokio::test]
async fn test_update_many_nested_delete_standard_forward() {
    let schema = r#"
        model Config {
            settings: String
            @@id(uuid)
        }
        
        model User {
            name: String
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
        db.execute("INSERT INTO Config (__id, settings) VALUES ('c1', 'shared')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, configId) VALUES ('u1', 'Alice', 'active', 'c1')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, configId) VALUES ('u2', 'Bob', 'active', 'c1')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "config": {
                "delete": true
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Config").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "Config should be deleted");
    }).await.unwrap();
}

#[tokio::test]
async fn test_update_many_nested_delete_polymorphic() {
    let schema = r#"
        base Content { }
        model Article extends Content { title: String @@id(uuid) }
        
        model User {
            name: String
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('a1', 'Shared')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, favorite_id, favorite_type) VALUES ('u1', 'Alice', 'active', 'a1', 'Article')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, favorite_id, favorite_type) VALUES ('u2', 'Bob', 'active', 'a1', 'Article')", []).unwrap();
        
        // Unrelated article
        db.execute("INSERT INTO Article (__id, title) VALUES ('a2', 'Unrelated')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "favorite": {
                "delete": { "where": { "__kind": "Article" } }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Article").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "Only the related article should be deleted");
        
        let title: String = db.query_row("SELECT title FROM Article", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Unrelated");
    }).await.unwrap();
}

#[tokio::test]
async fn test_update_many_nested_update_standard_forward() {
    let schema = r#"
        model Config {
            flag: Boolean
            @@id(uuid)
        }
        
        model User {
            name: String
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
        db.execute("INSERT INTO Config (__id, flag) VALUES ('c1', 0)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, configId) VALUES ('u1', 'Alice', 'active', 'c1')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, configId) VALUES ('u2', 'Bob', 'active', 'c1')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "config": {
                "update": { "where": {}, "data": { "flag": true } }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let flag: bool = db.query_row("SELECT flag FROM Config WHERE __id = 'c1'", [], |r| r.get(0)).unwrap();
        assert_eq!(flag, true);
    }).await.unwrap();
}

#[tokio::test]
async fn test_update_many_nested_update_polymorphic() {
    let schema = r#"
        base Content { }
        model Article extends Content { views: Int @@id(uuid) }
        
        model User {
            name: String
            status: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, views) VALUES ('a1', 10)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, favorite_id, favorite_type) VALUES ('u1', 'Alice', 'active', 'a1', 'Article')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status, favorite_id, favorite_type) VALUES ('u2', 'Bob', 'active', 'a1', 'Article')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "favorite": {
                "update": { "where": { "__kind": "Article" }, "data": { "views": 20 } }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let views: i64 = db.query_row("SELECT views FROM Article WHERE __id = 'a1'", [], |r| r.get(0)).unwrap();
        assert_eq!(views, 20);
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_create_regression_standard_forward() {
    let schema = r#"
        model Config {
            settings: String
            @@id(uuid)
        }
        
        model User {
            name: String
            configId: String?
            config: Config? @relation(onDelete: SetNull)
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "create",
        "data": {
            "name": "Alice",
            "config": {
                "create": { "settings": "local_config" }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Config").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        
        let c_id: String = db.query_row("SELECT configId FROM User WHERE name = 'Alice'", [], |r| r.get(0)).unwrap();
        assert!(c_id.len() > 0);
    }).await.unwrap();
}

#[tokio::test]
async fn test_singular_create_regression_polymorphic() {
    let schema = r#"
        base Content { }
        model Article extends Content { title: String @@id(uuid) }
        
        model User {
            name: String
            favorite: Content?
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "create",
        "data": {
            "name": "Alice",
            "favorite": {
                "create": { "__kind": "Article", "title": "My Article" }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT count(*) FROM Article").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        
        let (fid, ftype): (String, String) = db.query_row("SELECT favorite_id, favorite_type FROM User WHERE name = 'Alice'", [], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap();
        assert!(fid.len() > 0);
        assert_eq!(ftype, "Article");
    }).await.unwrap();
}

#[tokio::test]
async fn test_complex_chain_topology_bulk_singular_singular() {
    let schema = r#"
        model Metadata {
            info: String
            @@id(uuid)
        }
        
        model Config {
            settings: String
            metadataId: String?
            metadata: Metadata? @relation(onDelete: SetNull)
            @@id(uuid)
        }
        
        model User {
            name: String
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
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u1', 'Alice', 'active')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, status) VALUES ('u2', 'Bob', 'active')", []).unwrap();
    }).await.unwrap();

    let payload = json!({
        "model": "User",
        "action": "updateMany",
        "where": { "status": "active" },
        "data": {
            "config": {
                "create": { 
                    "settings": "global",
                    "metadata": {
                        "create": { "info": "meta-info" }
                    }
                }
            }
        }
    });

    let (status, res) = post_query(&app, payload).await;
    assert_eq!(status, StatusCode::OK, "Failed response: {:?}", res);

    conn.interact(|db| {
        let c_count: i64 = db.query_row("SELECT count(*) FROM Config", [], |r| r.get(0)).unwrap();
        assert_eq!(c_count, 1);
        
        let m_count: i64 = db.query_row("SELECT count(*) FROM Metadata", [], |r| r.get(0)).unwrap();
        assert_eq!(m_count, 1);
        
        let m_id: String = db.query_row("SELECT metadataId FROM Config", [], |r| r.get(0)).unwrap();
        assert!(m_id.len() > 0);
    }).await.unwrap();
}
