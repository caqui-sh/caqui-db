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
async fn test_e2e_on_delete() {
    let schema = r#"
        model User {
            name: String
            posts: Post[]
            profiles: Profile[]
            comments: Comment[]
            @@id(uuid)
        }
        
        model Post {
            title: String
            userId: String
            user: User @relation(fields: [userId], references: [__id], onDelete: Cascade)
            @@id(uuid)
        }
        
        model Profile {
            bio: String
            userId: String?
            user: User? @relation(fields: [userId], references: [__id], onDelete: SetNull)
            @@id(uuid)
        }
        
        model Comment {
            text: String
            userId: String
            user: User @relation(fields: [userId], references: [__id], onDelete: Restrict)
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);

    // 1. Test Cascade
    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "User",
        "data": { "name": "Alice" }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create user: {:?}", r);
    let u1_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "Post",
        "data": { "title": "Post 1", "userId": u1_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create post: {:?}", r);
    let p1_id = r["data"]["__id"].as_str().unwrap().to_string();
    
    let (s, r) = post_query(&app, json!({
        "action": "delete",
        "model": "User",
        "where": { "__id": u1_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to delete user: {:?}", r);
    
    let conn = pool.get().await.unwrap();
    let p1_id_owned = p1_id.clone();
    let count: i64 = conn.interact(move |db| {
        db.query_row("SELECT count(*) FROM Post WHERE __id = ?", [p1_id_owned], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(count, 0, "Post should have been cascaded");

    // 2. Test SetNull
    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "User",
        "data": { "name": "Bob" }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create user 2: {:?}", r);
    let u2_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "Profile",
        "data": { "bio": "Bio 1", "userId": u2_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create profile: {:?}", r);
    let pr1_id = r["data"]["__id"].as_str().unwrap().to_string();
    
    let (s, r) = post_query(&app, json!({
        "action": "delete",
        "model": "User",
        "where": { "__id": u2_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to delete user 2: {:?}", r);
    
    let pr1_id_owned = pr1_id.clone();
    let user_id: Option<String> = conn.interact(move |db| {
        db.query_row("SELECT userId FROM Profile WHERE __id = ?", [pr1_id_owned], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(user_id, None, "Profile userId should have been set to NULL");

    // 3. Test Restrict
    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "User",
        "data": { "name": "Charlie" }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create user 3: {:?}", r);
    let u3_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "Comment",
        "data": { "text": "Comment 1", "userId": u3_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create comment: {:?}", r);
    
    let (status, response) = post_query(&app, json!({
        "action": "delete",
        "model": "User",
        "where": { "__id": u3_id }
    })).await;
    assert_ne!(status, StatusCode::OK, "Deletion should have been restricted");
    let err_msg = response["error"].as_str().or(response["message"].as_str()).unwrap_or("");
    assert!(err_msg.contains("FOREIGN KEY constraint failed"), "Got error: {}", err_msg);
}

#[tokio::test]
async fn test_e2e_self_referential_cascade() {
    let schema = r#"
        model Employee {
            name: String
            managerId: String?
            manager: Employee? @relation("Management", fields: [managerId], references: [__id], onDelete: Cascade)
            subordinates: Employee[] @relation("Management")
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);

    let (_, r) = post_query(&app, json!({ "action": "create", "model": "Employee", "data": { "name": "CEO" } })).await;
    let ceo_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (_, r) = post_query(&app, json!({ "action": "create", "model": "Employee", "data": { "name": "Manager", "managerId": ceo_id } })).await;
    let mgr_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (_, r) = post_query(&app, json!({ "action": "create", "model": "Employee", "data": { "name": "Intern", "managerId": mgr_id } })).await;
    let intern_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (_, r) = post_query(&app, json!({ "action": "create", "model": "Employee", "data": { "name": "Manager 2", "managerId": ceo_id } })).await;
    let mgr2_id = r["data"]["__id"].as_str().unwrap().to_string();
    
    let (s, r) = post_query(&app, json!({
        "action": "delete",
        "model": "Employee",
        "where": { "__id": mgr_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to delete MGR: {:?}", r);
    
    let conn = pool.get().await.unwrap();
    let intern_id_owned = intern_id.clone();
    let intern_count: i64 = conn.interact(move |db| {
        db.query_row("SELECT count(*) FROM Employee WHERE __id = ?", [intern_id_owned], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(intern_count, 0, "Intern should have been cascaded");
    
    let mgr2_id_owned = mgr2_id.clone();
    let mgr2_count: i64 = conn.interact(move |db| {
        db.query_row("SELECT count(*) FROM Employee WHERE __id = ?", [mgr2_id_owned], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(mgr2_count, 1, "Manager 2 should be untouched");
    
    let (s, r) = post_query(&app, json!({
        "action": "delete",
        "model": "Employee",
        "where": { "__id": ceo_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to delete CEO: {:?}", r);
    
    let mgr2_id_owned_2 = mgr2_id.clone();
    let mgr2_count_after: i64 = conn.interact(move |db| {
        db.query_row("SELECT count(*) FROM Employee WHERE __id = ?", [mgr2_id_owned_2], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(mgr2_count_after, 0, "Manager 2 should have been cascaded when CEO was deleted");
}

#[tokio::test]
async fn test_e2e_polymorphic_cascade_delete() {
    let schema = r#"
        base Content { }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Comment {
            text: String
            parent: Content
            @@id(uuid)
        }
    "#;
    let (app, _dir, db_uri) = setup_app(schema).await;
    let pool = api_layer::db::create_pool(&db_uri);

    let payload = json!({
        "action": "create",
        "model": "Comment",
        "data": {
            "text": "Great article!",
            "parent": {
                "Article": { "create": { "title": "Polymorphic Writes" } }
            }
        }
    });
    let (s, r) = post_query(&app, payload).await;
    assert_eq!(s, StatusCode::OK, "Failed to create comment with article: {:?}", r);

    // Fetch article ID
    let (_, r) = post_query(&app, json!({
        "action": "findMany",
        "model": "Article",
        "select": { "__id": true }
    })).await;
    let article_id = r["data"][0]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({
        "action": "delete",
        "model": "Article",
        "where": { "__id": article_id }
    })).await;
    assert_eq!(s, StatusCode::OK, "Failed to delete article: {:?}", r);

    let conn = pool.get().await.unwrap();
    let count_after: i64 = conn.interact(|db| {
        db.query_row("SELECT count(*) FROM Comment", [], |r| r.get(0))
    }).await.unwrap().expect("Query failed");
    assert_eq!(count_after, 0, "Comment should be deleted by application-level cascade");
}

#[tokio::test]
async fn test_e2e_on_delete_no_action() {
    let schema = r#"
        model Parent {
            @@id(uuid)
        }
        
        model Child {
            parentId: String
            parent: Parent @relation(fields: [parentId], references: [__id], onDelete: NoAction)
            @@id(uuid)
        }
    "#;
    let (app, _dir, _db_uri) = setup_app(schema).await;

    let (_, r) = post_query(&app, json!({ "action": "create", "model": "Parent", "data": {} })).await;
    let p1_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({ "action": "create", "model": "Child", "data": { "parentId": p1_id } })).await;
    assert_eq!(s, StatusCode::OK, "Failed to create child: {:?}", r);
    
    let (status, response) = post_query(&app, json!({
        "action": "delete",
        "model": "Parent",
        "where": { "__id": p1_id }
    })).await;
    assert_ne!(status, StatusCode::OK, "Deletion should have been blocked by NO ACTION");
    let err_msg = response["error"].as_str().or(response["message"].as_str()).unwrap_or("");
    assert!(err_msg.contains("FOREIGN KEY constraint failed"), "Got error: {}", err_msg);
}
