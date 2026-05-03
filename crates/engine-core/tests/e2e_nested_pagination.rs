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
        model User {
            name: String
            age: Int
            status: String
            deletedAt: String?
            posts: Post[]
            profile: Profile?
            @@id(uuid)
        }
        model Profile {
            bio: String
            userId: String
            user: User @relation(fields: [userId], references: [__id])
            @@id(uuid)
        }
        model Post {
            title: String
            published: Boolean
            authorId: String
            author: User @relation(fields: [authorId], references: [__id])
            comments: Comment[]
            @@id(uuid)
        }
        model Comment {
            text: String
            postId: String
            post: Post @relation(fields: [postId], references: [__id])
            @@id(uuid)
        }
        union SearchResult = User | Post
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
        // Users
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u1', 'User 1', 25, 'active', NULL)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u2', 'User 2', 30, 'active', NULL)", []).unwrap();

        // 5 Posts per user
        for i in 1..=5 {
            db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES (?, ?, 1, 'u1')", [format!("p1_{}", i), format!("Post 1-{}", i)]).unwrap();
            db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES (?, ?, 1, 'u2')", [format!("p2_{}", i), format!("Post 2-{}", i)]).unwrap();
        }

        // 5 Comments on Post 1 (p1_1)
        for i in 1..=5 {
            db.execute("INSERT INTO Comment (__id, text, postId) VALUES (?, ?, 'p1_1')", [format!("c1_{}", i), format!("Comment {}", i)]).unwrap();
        }
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
async fn test_standard_nested_limit() {
    let (app, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "orderBy": { "__id": "asc" },
        "select": {
            "__id": true,
            "posts": {
                "limit": 2,
                "orderBy": { "__id": "asc" },
                "select": {
                    "__id": true,
                    "comments": {
                        "limit": 2,
                        "orderBy": { "__id": "asc" },
                        "select": { "__id": true }
                    }
                }
            }
        }
    });

    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();

    assert_eq!(data.len(), 2);
    
    // User u1
    let u1_posts = data[0]["posts"].as_array().unwrap();
    assert_eq!(u1_posts.len(), 2);
    assert_eq!(u1_posts[0]["__id"], "p1_1");
    assert_eq!(u1_posts[1]["__id"], "p1_2");

    let p1_comments = u1_posts[0]["comments"].as_array().unwrap();
    assert_eq!(p1_comments.len(), 2);
    assert_eq!(p1_comments[0]["__id"], "c1_1");
    assert_eq!(p1_comments[1]["__id"], "c1_2");

    let p1_2_comments = u1_posts[1]["comments"].as_array().unwrap();
    assert_eq!(p1_2_comments.len(), 0);

    // User u2
    let u2_posts = data[1]["posts"].as_array().unwrap();
    assert_eq!(u2_posts.len(), 2);
    assert_eq!(u2_posts[0]["__id"], "p2_1");
    assert_eq!(u2_posts[1]["__id"], "p2_2");
}

#[tokio::test]
async fn test_nested_offset_and_slicing() {
    let (app, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "posts": {
                "limit": 3,
                "offset": 0,
                "orderBy": { "__id": "asc" },
                "select": {
                    "__id": true,
                    "comments": {
                        "limit": 2,
                        "skip": 2,
                        "orderBy": { "__id": "asc" },
                        "select": { "__id": true }
                    }
                }
            }
        }
    });

    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();
    
    let u1_posts = data[0]["posts"].as_array().unwrap();
    assert_eq!(u1_posts[0]["__id"], "p1_1");

    let p1_comments = u1_posts[0]["comments"].as_array().unwrap();
    assert_eq!(p1_comments.len(), 2);
    assert_eq!(p1_comments[0]["__id"], "c1_3");
    assert_eq!(p1_comments[1]["__id"], "c1_4");
}

#[tokio::test]
async fn test_deeply_nested_pagination_level_3() {
    let (app, _dir) = setup_app().await;

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "posts": {
                "limit": 1,
                "orderBy": { "__id": "asc" },
                "select": {
                    "__id": true,
                    "comments": {
                        "limit": 2,
                        "orderBy": { "__id": "asc" },
                        "select": {
                            "__id": true,
                            "post": {
                                "select": {
                                    "__id": true,
                                    "author": {
                                        "select": { "__id": true }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();

    let u1_posts = data[0]["posts"].as_array().unwrap();
    let p1_1 = &u1_posts[0];
    assert_eq!(p1_1["__id"], "p1_1");

    let comments = p1_1["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);

    let c1 = &comments[0];
    assert_eq!(c1["__id"], "c1_1");
    assert_eq!(c1["post"]["__id"], "p1_1");
    assert_eq!(c1["post"]["author"]["__id"], "u1");
}
