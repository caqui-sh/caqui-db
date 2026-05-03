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

async fn setup_app() -> (axum::Router, tempfile::TempDir) {
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
        model Post {
            title: String
            @@id(uuid)
        }
        model Video {
            url: String
            @@id(uuid)
        }
        union Content = Post | Video | User
        model User {
            contents: Content[]
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
        db.execute("INSERT INTO Post (__id, title) VALUES ('p1', 'Hello World')", []).unwrap();
        db.execute("INSERT INTO Video (__id, url) VALUES ('v1', 'http://example.com/video')", []).unwrap();
        
        // Scenario A: Standard Hydration
        db.execute("INSERT INTO User (__id, contents) VALUES ('u1', '[{\"type\":\"Post\",\"__id\":\"p1\"}, {\"type\":\"Video\",\"__id\":\"v1\"}]')", []).unwrap();
        
        // Scenario B: Empty State Execution (NULL)
        db.execute("INSERT INTO User (__id, contents) VALUES ('u2', NULL)", []).unwrap();
        
        // Scenario C: Schema Evolution (Legacy/Unknown Discriminator)
        db.execute("INSERT INTO User (__id, contents) VALUES ('u3', '[{\"type\":\"UnknownType\",\"__id\":\"99\"}, {\"type\":\"Post\",\"__id\":\"p1\"}]')", []).unwrap();

        // Scenario D: Recursive Scoping
        db.execute("INSERT INTO User (__id, contents) VALUES ('u4', '[{\"type\":\"User\",\"__id\":\"u5\"}]')", []).unwrap();
        db.execute("INSERT INTO User (__id, contents) VALUES ('u5', '[{\"type\":\"Post\",\"__id\":\"p1\"}]')", []).unwrap();
    }).await.unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let state = api_layer::state::EngineState { 
        ast: Arc::new(ast), 
        db_pool: pool 
    };
    let app = api_layer::router::build_dynamic_router(state);
    
    (app, dir)
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
async fn test_e2e_union_array_standard_hydration() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "contents": {
                "Post": { "select": { "title": true } },
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u1");
    let contents = user["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["title"], "Hello World");
    assert_eq!(contents[1]["url"], "http://example.com/video");
}

#[tokio::test]
async fn test_e2e_union_array_empty_state() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u2" },
        "select": {
            "__id": true,
            "contents": {
                "Post": { "select": { "title": true } },
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u2");
    let contents = &user["contents"];
    assert!(contents.is_null() || contents.as_array().map_or(false, |a| a.is_empty()));
}

#[tokio::test]
async fn test_e2e_union_array_legacy_discriminator() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u3" },
        "select": {
            "__id": true,
            "contents": {
                "Post": { "select": { "title": true } },
                "Video": { "select": { "url": true } }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u3");
    let contents = user["contents"].as_array().unwrap();
    
    assert_eq!(contents.len(), 2);
    assert!(contents[0].is_null());
    assert_eq!(contents[1]["title"], "Hello World");
}

#[tokio::test]
async fn test_e2e_union_array_recursive_scoping() {
    let (app, _dir) = setup_app().await;
    
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u4" },
        "select": {
            "__id": true,
            "contents": {
                "User": {
                    "select": {
                        "__id": true,
                        "contents": {
                            "Post": { "select": { "title": true } }
                        }
                    }
                }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    let user = &users[0];
    
    assert_eq!(user["__id"], "u4");
    let contents = user["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    
    let inner_user = &contents[0];
    assert_eq!(inner_user["__id"], "u5");
    
    let inner_contents = inner_user["contents"].as_array().unwrap();
    assert_eq!(inner_contents.len(), 1);
    assert_eq!(inner_contents[0]["title"], "Hello World");
}
