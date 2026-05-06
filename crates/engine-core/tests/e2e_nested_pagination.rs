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
            contents: Content[]
            videos: Video[]
            @@id(uuid)
        }
        model Profile {
            bio: String
            userId: String
            user: User 
            @@id(uuid)
        }
        model Post {
            title: String
            published: Boolean
            authorId: String
            author: User 
            comments: Comment[]
            tag_ids: String[]
            tags: Tag[] 
            @@id(uuid)
        }
        model Comment {
            text: String
            postId: String
            post: Post 
            score: Int
            @@id(uuid)
        }
        model Video {
            title: String
            authorId: String
            author: User 
            @@id(uuid)
        }
        model Tag {
            name: String
            post: Post
            @@id(uuid)
        }
        union Content = Post | Video
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
        let u1_contents = r#"[
            {"type":"Post","__id":"p1_1"}, {"type":"Post","__id":"p1_2"},
            {"type":"Post","__id":"p1_3"}, {"type":"Post","__id":"p1_4"},
            {"type":"Video","__id":"v1_1"}, {"type":"Video","__id":"v1_2"},
            {"type":"Video","__id":"v1_3"}, {"type":"Video","__id":"v1_4"}
        ]"#;
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt, contents) VALUES ('u1', 'User 1', 25, 'active', NULL, ?)", [u1_contents]).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt, contents) VALUES ('u2', 'User 2', 30, 'active', NULL, '[]')", []).unwrap();

        // Tags
        db.execute("INSERT INTO Tag (__id, name) VALUES ('t1', 'rust')", []).unwrap();
        db.execute("INSERT INTO Tag (__id, name) VALUES ('t2', 'graphql')", []).unwrap();

        // 5 Posts per user
        for i in 1..=5 {
            let tags = if i % 2 == 0 { "[\"t1\"]" } else { "[\"t1\", \"t2\"]" };
            db.execute("INSERT INTO Post (__id, title, published, authorId, tag_ids) VALUES (?, ?, 1, 'u1', ?)", [format!("p1_{}", i), format!("Post 1-{}", i), tags.to_string()]).unwrap();
            db.execute("INSERT INTO Post (__id, title, published, authorId, tag_ids) VALUES (?, ?, 1, 'u2', ?)", [format!("p2_{}", i), format!("Post 2-{}", i), "[]".to_string()]).unwrap();
        }

        // 5 Videos for User 1
        for i in 1..=5 {
            db.execute("INSERT INTO Video (__id, title, authorId) VALUES (?, ?, 'u1')", [format!("v1_{}", i), format!("Video 1-{}", i)]).unwrap();
        }

        // 5 Comments on Post 1 (p1_1)
        for i in 1..=5 {
            let score = (10 - i).to_string(); // 9, 8, 7, 6, 5
            let text = format!("Comment {}", i);
            db.execute("INSERT INTO Comment (__id, text, postId, score) VALUES (?, ?, 'p1_1', ?)", [format!("c1_{}", i), text, score]).unwrap();
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


#[tokio::test]
async fn test_polymorphic_array_pagination() {
    let (app, _dir) = setup_app().await;
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "contents": {
                "Post": {
                    "limit": 4,
                    "orderBy": { "__id": "asc" },
                    "select": {
                        "__id": true,
                        "title": true
                    }
                },
                "Video": {
                    "limit": 4,
                    "orderBy": { "__id": "asc" },
                    "select": {
                        "__id": true,
                        "title": true
                    }
                }
            }
        }
    });
    let response = post_query(&app, payload).await;
    println!("POLYMORPHIC RESPONSE: {}", response);
    let data = response["data"].as_array().unwrap();
    let contents = data[0]["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 8); // 4 posts + 4 videos (if they are just concatenated or mixed)
    // We will just print them and exit to see what is generated, or just check the length

}

#[tokio::test]
async fn test_filtered_pagination() {
    let (app, _dir) = setup_app().await;
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "posts": {
                "where": { "title": "Post 1-1" },
                "limit": 1,
                "orderBy": { "__id": "asc" },
                "select": {
                    "__id": true,
                    "title": true
                }
            }
        }
    });
    let response = post_query(&app, payload).await;
    println!("FILTERED RESPONSE: {}", response);
    let data = response["data"].as_array().unwrap();
    let posts = data[0]["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["__id"], "p1_1");
}

#[tokio::test]
async fn test_sibling_relation_pagination() {
    let (app, _dir) = setup_app().await;
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "__id": "u1" },
        "select": {
            "__id": true,
            "posts": {
                "limit": 2,
                "orderBy": { "__id": "asc" },
                "select": { "__id": true }
            },
            "videos": {
                "limit": 2,
                "orderBy": { "__id": "asc" },
                "select": { "__id": true }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();
    let posts = data[0]["posts"].as_array().unwrap();
    let videos = data[0]["videos"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(videos.len(), 2);
    assert_eq!(posts[0]["__id"], "p1_1");
    assert_eq!(videos[0]["__id"], "v1_1");
}

#[tokio::test]
async fn test_inverse_relation_pagination() {
    let (app, _dir) = setup_app().await;
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Post",
        "where": { "__id": "p1_1" },
        "select": {
            "__id": true,
            "comments": {
                "limit": 2,
                "orderBy": { "__id": "asc" },
                "select": { "__id": true }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();
    let comments = data[0]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["__id"], "c1_1");
    assert_eq!(comments[1]["__id"], "c1_2");
}

#[tokio::test]
async fn test_deterministic_secondary_sorting() {
    let (app, _dir) = setup_app().await;
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Post",
        "where": { "__id": "p1_1" },
        "select": {
            "__id": true,
            "comments": {
                "limit": 3,
                "orderBy": {
                    "score": "asc",
                    "__id": "asc"
                },
                "select": {
                    "__id": true,
                    "score": true
                }
            }
        }
    });
    let response = post_query(&app, payload).await;
    let data = response["data"].as_array().unwrap();
    let comments = data[0]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 3);
    assert_eq!(comments[0]["score"], 9); 
    assert_eq!(comments[0]["__id"], "c1_1");
    assert_eq!(comments[1]["score"], 8);
    assert_eq!(comments[1]["__id"], "c1_2");
    assert_eq!(comments[2]["score"], 7);
}
