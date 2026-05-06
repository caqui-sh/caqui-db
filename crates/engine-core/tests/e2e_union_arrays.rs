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

#[tokio::test]
async fn test_polymorphic_union_base_shape_filtering() {
    let schema = r#"
        // 1. Define distinct base shapes
        base Timestamped {
            updatedAt: String @default("never")
        }
        base Viewable {
            views: Int @default(0)
        }

        // 2. Define concrete models with mixed inheritance
        model Article extends Timestamped, Viewable {
            body: String
            collectionId: String?
            collection: Collection? 
            @@id(uuid)
        }

        model Video extends Viewable {
            url: String
            collectionId: String?
            collection: Collection? 
            @@id(uuid)
        }

        model User extends Timestamped {
            name: String
            collectionId: String?
            collection: Collection? 
            @@id(uuid)
        }

        // 3. Define the Union and Parent Model
        union SearchResult = Article | Video | User

        model Collection {
            name: String
            items: SearchResult[]
            @@id(uuid)
        }
    "#;

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

    // Seed Data
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO Collection (__id, name) VALUES ('c1', 'My Collection')", []).unwrap();
        
        db.execute("INSERT INTO Article (__id, body, collectionId, updatedAt, views) VALUES ('a1', 'Body', 'c1', 'never', 0)", []).unwrap();
        db.execute("INSERT INTO Video (__id, url, collectionId, views) VALUES ('v1', 'url', 'c1', 0)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, collectionId, updatedAt) VALUES ('u1', 'Alice', 'c1', 'never')", []).unwrap();
    }).await.unwrap();

    let payload = serde_json::json!({
        "action": "update",
        "model": "Collection",
        "where": { "__id": "c1" },
        "data": {
            "items": {
                "updateMany": {
                    "where": { "__Timestamped": true },
                    "data": { "updatedAt": "today" }
                }
            }
        }
    });

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

    // Verify Data
    conn.interact(|db| {
        let art_updated_at: String = db.query_row("SELECT updatedAt FROM Article WHERE __id = 'a1'", [], |r| r.get(0)).unwrap();
        assert_eq!(art_updated_at, "today");

        let user_updated_at: String = db.query_row("SELECT updatedAt FROM User WHERE __id = 'u1'", [], |r| r.get(0)).unwrap();
        assert_eq!(user_updated_at, "today");

        // Video shouldn't have been updated (nor errored)
        let vid_views: i64 = db.query_row("SELECT views FROM Video WHERE __id = 'v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(vid_views, 0);
    }).await.unwrap();
}
