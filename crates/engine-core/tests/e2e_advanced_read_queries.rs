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
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u1', 'Alice', 25, 'active', NULL)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u2', 'Bob', 30, 'active', NULL)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u3', 'Charlie', 22, 'inactive', '2023-01-01')", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u4', 'Dave', 19, 'active', NULL)", []).unwrap();
        db.execute("INSERT INTO User (__id, name, age, status, deletedAt) VALUES ('u5', 'Admin', 40, 'active', NULL)", []).unwrap();

        // Profiles
        db.execute("INSERT INTO Profile (__id, bio, userId) VALUES ('pr1', 'Rustacean', 'u1')", []).unwrap();
        db.execute("INSERT INTO Profile (__id, bio, userId) VALUES ('pr2', 'Gopher', 'u2')", []).unwrap();

        // Posts
        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p1', 'Alice Post 1', 1, 'u1')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p2', 'Alice Post 2', 0, 'u1')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, published, authorId) VALUES ('p3', 'Bob Post 1', 1, 'u2')", []).unwrap();

        // Comments
        db.execute("INSERT INTO Comment (__id, text, postId) VALUES ('c1', 'Great post!', 'p1')", []).unwrap();
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
async fn test_advanced_scalar_operators() {
    let (app, _dir) = setup_app().await;

    // 1. Range: age >= 20 AND age < 30 (Alice: 25, Charlie: 22)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "age": { "gte": 20, "lt": 30 } },
        "orderBy": { "name": "asc" },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["name"], "Alice");
    assert_eq!(users[1]["name"], "Charlie");

    // 2. Inclusion: status IN ["inactive"]
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "status": { "in": ["inactive"] } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["name"], "Charlie");

    // 3. Null Checks: deletedAt IS NULL
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "deletedAt": null },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 4); // Alice, Bob, Dave, Admin

    // 4. Negation: name != "Admin"
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "notEq": "Admin" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert!(users.iter().all(|u| u["name"] != "Admin"));
}

#[tokio::test]
async fn test_deep_relational_filtering() {
    let (app, _dir) = setup_app().await;

    // 1. some: Users who have some published posts (Alice, Bob)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "posts": { "some": { "published": true } } },
        "orderBy": { "name": "asc" },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["name"], "Alice");
    assert_eq!(users[1]["name"], "Bob");

    // 2. none: Users who have no posts (Charlie, Dave, Admin)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "posts": { "none": {} } },
        "orderBy": { "name": "asc" },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 3);
    assert_eq!(users[0]["name"], "Admin");
    assert_eq!(users[1]["name"], "Charlie");
    assert_eq!(users[2]["name"], "Dave");

    // 3. is: Users whose profile bio is "Rustacean" (Alice)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "profile": { "is": { "bio": "Rustacean" } } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["name"], "Alice");
}

#[tokio::test]
async fn test_logical_grouping() {
    let (app, _dir) = setup_app().await;

    // OR: age < 20 OR status == "inactive" (Dave: 19, Charlie: inactive)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": {
            "OR": [
                { "age": { "lt": 20 } },
                { "status": "inactive" }
            ]
        },
        "orderBy": { "name": "asc" },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let users = response["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["name"], "Charlie");
    assert_eq!(users[1]["name"], "Dave");
}

#[tokio::test]
async fn test_polymorphic_union_http_reads() {
    let (app, _dir) = setup_app().await;

    // findMany on SearchResult union
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "SearchResult",
        "select": {
            "User": { "select": { "name": true, "__kind": true } },
            "Post": { "select": { "title": true, "__kind": true } }
        }
    });
    
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    
    // We expect both Users and Posts in the result
    assert!(items.len() >= 8); // 5 users + 3 posts
    
    let alice = items.iter().find(|i| i["name"] == "Alice").unwrap();
    assert_eq!(alice["__kind"], "User");
    
    let post1 = items.iter().find(|i| i["title"] == "Alice Post 1").unwrap();
    assert_eq!(post1["__kind"], "Post");
}

#[tokio::test]
async fn test_advanced_string_filtering() {
    let (app, _dir) = setup_app().await;

    let names = vec![
        "Apple",
        "Application",
        "Snapple",
        "Banana",
        "100% Juice",
        "Apple_Pie",
        "Back\\Slash",
    ];

    for name in names {
        let payload = serde_json::json!({
            "action": "create",
            "model": "User",
            "data": {
                "name": name,
                "age": 25,
                "status": "active"
            },
            "select": { "__id": true }
        });
        post_query(&app, payload).await;
    }

    // 1. startsWith
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "startsWith": "App" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 3);

    // 2. contains
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "contains": "ppl" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 4);

    // 3. endsWith
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "endsWith": "le" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 2);

    // 4. Escape %
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "contains": "100%" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "100% Juice");

    // 5. Escape _
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "contains": "e_P" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "Apple_Pie");

    // 6. Escape \
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "contains": "k\\S" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "Back\\Slash");

    // 7. Validation Rejection (contains on Int)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "age": { "contains": "2" } },
        "select": { "name": true }
    });
    
    // We can't use post_query directly because it asserts StatusCode::OK
    let req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 8. Case-Insensitivity (SQLite default LIKE behavior)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": { "name": { "startsWith": "app" } },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 3);

    // 9. Compound String Filtering
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "User",
        "where": {
            "OR": [
                { "name": { "startsWith": "App" } },
                { "name": { "endsWith": "le" } }
            ]
        },
        "select": { "name": true }
    });
    let response = post_query(&app, payload).await;
    let items = response["data"].as_array().unwrap();
    // App -> Apple, Application, Apple_Pie
    // le -> Apple, Snapple
    // Union -> Apple, Application, Apple_Pie, Snapple -> 4
    assert_eq!(items.len(), 4);
}

#[tokio::test]
async fn test_fulltext_search() {
    let (app, _dir) = setup_app().await;

    // The User model in setup_app() doesn't have @@fulltext yet, so we need a dedicated setup
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let _ = engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = std::env::var("CARGO_BIN_EXE_caqui").unwrap();
    let mut cmd1 = Command::new("git");
    cmd1.arg("init").current_dir(workspace);
    run_cmd(cmd1);

    let schema = r#"
        model Document {
            title: String
            body: String
            @@fulltext([title, body])
            @@id(uuid)
        }
    "#;
    std::fs::write(workspace.join("schema.cq"), schema).unwrap();
    
    let mut cmd3 = Command::new(&caqui_bin);
    cmd3.arg("schema").arg("migrate").current_dir(workspace);
    run_cmd(cmd3);

    let schema_str = std::fs::read_to_string(workspace.join("schema.cq")).unwrap();
    let ast = schema_parser::parse_schema(&schema_str).unwrap();
    let ast = schema_parser::validate_schema(ast).unwrap();
    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let pool = api_layer::db::create_pool(&db_uri);
    let state = api_layer::state::EngineState { ast: Arc::new(ast), db_pool: pool };
    let app_fts = api_layer::router::build_dynamic_router(state);

    let docs = vec![
        ("Rust Programming", "The quick brown fox"),
        ("Python Guide", "The lazy dog"),
    ];

    let mut doc_ids = std::collections::HashMap::new();
    for (title, body) in docs {
        let payload = serde_json::json!({
            "action": "create",
            "model": "Document",
            "data": { "title": title, "body": body },
            "select": { "__id": true }
        });
        let resp = post_query(&app_fts, payload).await;
        let id = resp["data"]["__id"].as_str().unwrap().to_string();
        doc_ids.insert(title.to_string(), id);
    }

    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "brown",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Programming");

    // 2. Multi-column match (search in title)
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "Rust",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Programming");

    // 3. FTS5 Boolean OR syntax
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "quick OR lazy",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Rust Programming"));
    assert!(titles.contains(&"Python Guide"));

    // 4. Missing Term
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "missing_word",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 0);

    // 5. Prefix Matching
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "prog*",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Programming");

    // 6. Phrase Matching
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "\"brown fox\"",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Programming");

    // 7. Update Synchronization (_fts_au trigger)
    let rust_id = doc_ids.get("Rust Programming").unwrap();
    let payload = serde_json::json!({
        "action": "update",
        "model": "Document",
        "where": { "__id": rust_id },
        "data": { "body": "The slow black cat" },
        "select": { "__id": true }
    });
    post_query(&app_fts, payload).await;

    // Verify old term is gone
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "brown",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 0);

    // Verify new term is present
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "black",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Programming");

    // 8. Delete Synchronization (_fts_ad trigger)
    let python_id = doc_ids.get("Python Guide").unwrap();
    let payload = serde_json::json!({
        "action": "delete",
        "model": "Document",
        "where": { "__id": python_id },
        "select": { "__id": true }
    });
    post_query(&app_fts, payload).await;

    // Verify deleted document is removed from FTS index
    let payload = serde_json::json!({
        "action": "findMany",
        "model": "Document",
        "search": "Python",
        "select": { "title": true }
    });
    let response = post_query(&app_fts, payload).await;
    let items = response["data"].as_array().unwrap();
    assert_eq!(items.len(), 0);
}
