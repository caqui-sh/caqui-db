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

async fn post_query(app: &axum::Router, payload: Value) -> Value {
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
    if status != StatusCode::OK {
        panic!("Request failed with status {}: {:?}", status, String::from_utf8_lossy(&body_bytes));
    }
    serde_json::from_slice(&body_bytes).unwrap()
}

#[tokio::test]
async fn test_e2e_polymorphic_union_reads() {
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
        base Employee { department: String }
        model Engineer extends Employee { language: String @@id(uuid) }
        model Manager extends Employee { directReports: Int @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Engineer (__id, department, language) VALUES ('1', 'Engineering', 'Rust')", []).unwrap();
        db.execute("INSERT INTO Manager (__id, department, directReports) VALUES ('2', 'Sales', 5)", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Employee",
        "select": { "__id": true, "department": true }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();

    assert_eq!(rows.len(), 2);

    let eng_row = rows.iter().find(|r| r["department"] == "Engineering").unwrap();
    assert_eq!(eng_row["department"], "Engineering");
    assert!(eng_row.get("language").is_none());

    let mgr_row = rows.iter().find(|r| r["department"] == "Sales").unwrap();
    assert_eq!(mgr_row["department"], "Sales");
    assert!(mgr_row.get("directReports").is_none());

    // Test Filtering
    let filtered_payload = serde_json::json!({
        "action": "findMany",
        "model": "Employee",
        "select": { "department": true },
        "where": { "department": "Engineering" }
    });
    let response = post_query(&app, filtered_payload).await;
    let filter_rows = response["data"].as_array().unwrap();
    
    assert_eq!(filter_rows.len(), 1);
    assert_eq!(filter_rows[0]["department"], "Engineering");
}

#[tokio::test]
async fn test_e2e_nested_polymorphic_relations() {
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
        base Employee { teamId: String  }
        model Engineer extends Employee { language: String @@id(uuid) }
        model Manager extends Employee { directReports: Int @@id(uuid) }
        
        model Team {
            name: String
            members: Employee[] @relation(fields: [teamId], references: [__id])
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
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Team (__id, name) VALUES ('t1', 'Platform')", []).unwrap();
        db.execute("INSERT INTO Engineer (__id, teamId, language) VALUES ('e1', 't1', 'Rust')", []).unwrap();
        db.execute("INSERT INTO Manager (__id, teamId, directReports) VALUES ('m1', 't1', 5)", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Team",
        "select": { 
            "name": true,
            "members": {
                "Engineer": { "select": { "__id": true, "teamId": true, "language": true, "__kind": true } },
                "Manager": { "select": { "__id": true, "teamId": true, "directReports": true, "__kind": true } }
            }
        }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "Platform");
    
    let members = rows[0]["members"].as_array().expect("members must be array");
    
    if members.len() > 0 {
        let engineer = members.iter().find(|m| m.get("language").is_some()).unwrap();
        assert_eq!(engineer["__id"], "e1");
        assert_eq!(engineer["__kind"], "Engineer");
        
        let manager = members.iter().find(|m| m.get("directReports").is_some()).unwrap();
        assert_eq!(manager["__id"], "m1");
        assert_eq!(manager["__kind"], "Manager");
    }
}

#[tokio::test]
async fn test_e2e_polymorphic_filtering() {
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
        base Content {  }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Comment {
            text: String
            parent: Content @relation(fields: [parent_type, parent_id], references: [__kind, __id])
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
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Article (__id, title) VALUES ('a1', 'Match')", []).unwrap();
        db.execute("INSERT INTO Article (__id, title) VALUES ('a2', 'No Match')", []).unwrap();
        db.execute("INSERT INTO Video (__id, duration) VALUES ('v1', 120)", []).unwrap();
        
        db.execute("INSERT INTO Comment (__id, text, parent_type, parent_id) VALUES ('c1', 'C1', 'Article', 'a1')", []).unwrap();
        db.execute("INSERT INTO Comment (__id, text, parent_type, parent_id) VALUES ('c2', 'C2', 'Article', 'a2')", []).unwrap();
        db.execute("INSERT INTO Comment (__id, text, parent_type, parent_id) VALUES ('c3', 'C3', 'Video', 'v1')", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Comment",
        "where": {
            "parent": {
                "Article": {
                    "title": "Match"
                }
            }
        },
        "select": {
            "__id": true,
            "text": true
        }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["__id"], "c1");
    assert_eq!(rows[0]["text"], "C1");
}

#[tokio::test]
async fn test_e2e_diamond_inheritance() {
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
        base Timestamped { createdAt: String  }
        base Node {  }
        base Record extends Node, Timestamped {}
        model Post extends Record { text: String @@id(uuid) }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    
    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Post (__id, createdAt, text) VALUES ('post_1', '2023-01-01', 'Deep Diamond')", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);

    // Query abstract Node
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Node",
        "select": { "__id": true, "__Timestamped": true, "__Record": true, "__Node": true }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["__id"], "post_1");
    assert_eq!(rows[0]["__Node"], true);
    assert_eq!(rows[0]["__Timestamped"], true);
    assert_eq!(rows[0]["__Record"], true);
}
