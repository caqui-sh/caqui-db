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
async fn test_e2e_relation_names() {
    let schema = r#"
        model User {
            name: String
            authoredPosts: Post[] @relation("AuthorToPost")
            reviewedPosts: Post[] @relation("ReviewerToPost")
            @@id(uuid)
        }
        
        model Post {
            title: String
            authorId: String
            author: User @relation("AuthorToPost")
            reviewerId: String?
            reviewer: User? @relation("ReviewerToPost")
            @@id(uuid)
        }
    "#;

    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            INSERT INTO User (__id, name) VALUES ('u1', 'Alice');
            INSERT INTO User (__id, name) VALUES ('u2', 'Bob');
            
            -- Alice authors Post 1, Bob reviews it
            INSERT INTO Post (__id, title, authorId, reviewerId) VALUES ('p1', 'Rust Guide', 'u1', 'u2');
            
            -- Bob authors Post 2, Alice reviews it
            INSERT INTO Post (__id, title, authorId, reviewerId) VALUES ('p2', 'SQLite Tips', 'u2', 'u1');
            
            -- Alice authors Post 3, no reviewer
            INSERT INTO Post (__id, title, authorId, reviewerId) VALUES ('p3', 'Zero Overhead', 'u1', NULL);
        ").unwrap();
    }).await.unwrap();

    let payload = json!({
        "action": "findMany",
        "model": "User",
        "select": {
            "name": true,
            "authoredPosts": {
                "select": { "title": true }
            },
            "reviewedPosts": {
                "select": { "title": true }
            }
        },
        "where": {
            "name": "Alice"
        }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();
    
    assert_eq!(rows.len(), 1, "Should return exactly one user (Alice)");
    let alice = &rows[0];
    assert_eq!(alice["name"], "Alice");
    
    // Assert Authored Posts resolved correctly (AuthorToPost -> authorId)
    let authored = alice["authoredPosts"].as_array().unwrap();
    assert_eq!(authored.len(), 2, "Alice authored 2 posts");
    let titles: Vec<_> = authored.iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Rust Guide"));
    assert!(titles.contains(&"Zero Overhead"));
    
    // Assert Reviewed Posts resolved correctly (ReviewerToPost -> reviewerId)
    let reviewed = alice["reviewedPosts"].as_array().unwrap();
    assert_eq!(reviewed.len(), 1, "Alice reviewed 1 post");
    assert_eq!(reviewed[0]["title"], "SQLite Tips");
}

#[tokio::test]
async fn test_e2e_self_referential_relations() {
    let schema = r#"
        model Employee {
            name: String
            managerId: String?
            manager: Employee? @relation("ManagerToEmployee")
            directReports: Employee[] @relation("ManagerToEmployee")
            @@id(uuid)
        }
    "#;

    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);
    
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            INSERT INTO Employee (__id, name, managerId) VALUES ('e1', 'CEO', 'e1');
            INSERT INTO Employee (__id, name, managerId) VALUES ('e2', 'VP', 'e1');
            INSERT INTO Employee (__id, name, managerId) VALUES ('e3', 'Manager', 'e2');
            INSERT INTO Employee (__id, name, managerId) VALUES ('e4', 'IC', 'e3');
        ").unwrap();
    }).await.unwrap();

    let payload = json!({
        "action": "findMany",
        "model": "Employee",
        "select": {
            "name": true,
            "directReports": {
                "select": {
                    "name": true,
                    "directReports": {
                        "select": {
                            "name": true,
                            "manager": {
                                "select": { "name": true }
                            }
                        }
                    }
                },
                "where": {
                    "name": { "notEq": "CEO" }
                }
            }
        },
        "where": {
            "name": "CEO"
        }
    });

    let response = post_query(&app, payload).await;
    let rows = response["data"].as_array().unwrap();
    
    assert_eq!(rows.len(), 1, "Should return exactly one CEO");
    let ceo = &rows[0];
    assert_eq!(ceo["name"], "CEO");
    
    let ceo_directs = ceo["directReports"].as_array().unwrap();
    assert_eq!(ceo_directs.len(), 1, "CEO has 1 real direct report (VP)");
    assert_eq!(ceo_directs[0]["name"], "VP");
    
    let vp_directs = ceo_directs[0]["directReports"].as_array().unwrap();
    assert_eq!(vp_directs.len(), 1, "VP has 1 direct report (Manager)");
    assert_eq!(vp_directs[0]["name"], "Manager");
    
    let manager_manager = &vp_directs[0]["manager"];
    assert_eq!(manager_manager["name"], "VP", "Manager's manager is VP");
}
