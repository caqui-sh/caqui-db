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
    println!("CMD: {:?}\nSTDOUT: {}\nSTDERR: {}", cmd, stdout, stderr);
    if !output.status.success() {
        panic!("Command {:?} failed!\nstdout: {}\nstderr: {}", cmd, stdout, stderr);
    }
    stdout
}

async fn setup_app_with_schema(schema: &str) -> (axum::Router, deadpool_sqlite::Pool, tempfile::TempDir) {
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
    
    (app, pool, dir)
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
    let body_str = String::from_utf8_lossy(&body_bytes);
    if status != StatusCode::OK {
        println!("API ERROR ({}): {}", status, body_str);
    }
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
        json!({ "error": body_str })
    });
    (status, body_val)
}

#[tokio::test]
async fn test_singular_set_reject_dead_keywords() {
    let schema = r#"
        model Parent {
            name: String
            child: Child? @relation("ParentChild")
            @@id(uuid)
        }
        model Child {
            name: String
            parentId: String? @unique
            parent: Parent? @relation("ParentChild")
            @@id(uuid)
        }
    "#;
    let (app, _, _dir) = setup_app_with_schema(schema).await;

    // 1. Reject 'connect'
    let (status, res) = post_query(&app, json!({
        "action": "create",
        "model": "Parent",
        "data": {
            "name": "P1",
            "child": { "connect": { "__id": "c1" } }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: 'connect' is not allowed on singular relation 'child'. Use 'set' instead."));

    // 2. Reject 'disconnect'
    let (status, res) = post_query(&app, json!({
        "action": "update",
        "model": "Parent",
        "where": { "__id": "p1" },
        "data": {
            "child": { "disconnect": true }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: 'disconnect' is not allowed on singular relation 'child'. Use 'set' instead."));

    // 3. Array Rejection on Singular 'set'
    let (status, res) = post_query(&app, json!({
        "action": "create",
        "model": "Parent",
        "data": {
            "name": "P1",
            "child": { "set": [{ "__id": "c1" }] }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: 'set' payload for singular relation 'child' must be an object or null."));
}

#[tokio::test]
async fn test_singular_set_implicit_disconnect_and_deletion_lifecycle() {
    let schema = r#"
        model User {
            name: String
            profile: Profile?
            avatar: Avatar? @relation(onDisconnect: Delete)
            @@id(uuid)
        }
        model Profile {
            bio: String
            userId: String? @unique
            user: User?
            @@id(uuid)
        }
        model Avatar {
            url: String
            userId: String? @unique
            user: User?
            @@id(uuid)
        }
    "#;
    let (app, pool, _dir) = setup_app_with_schema(schema).await;

    // Setup User with Profile and Avatar
    let (status, res) = post_query(&app, json!({
        "action": "create",
        "model": "User",
        "data": {
            "name": "Alice",
            "profile": { "create": { "bio": "Hello" } },
            "avatar": { "create": { "url": "img1" } }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);
    let user_id = res["data"]["__id"].as_str().unwrap().to_string();

    let conn = pool.get().await.unwrap();
    let (prof_id, avatar_id): (String, String) = conn.interact({
        let user_id = user_id.clone();
        move |db| {
            let mut stmt = db.prepare("SELECT __id, bio, userId FROM Profile").unwrap();
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0).unwrap();
                let bio: String = row.get(1).unwrap();
                let u_id: Option<String> = row.get(2).unwrap();
                Ok((id, bio, u_id))
            }).unwrap();
            for r in rows {
                println!("PROFILE ROW: {:?}", r.unwrap());
            }

            let p: String = db.query_row("SELECT __id FROM Profile WHERE userId = ?", [&user_id], |r| r.get(0)).unwrap();
            let a: String = db.query_row("SELECT __id FROM Avatar WHERE userId = ?", [&user_id], |r| r.get(0)).unwrap();
            (p, a)
        }
    }).await.unwrap();

    // 1. Implicit Disconnect (Nullification)
    let (status, _) = post_query(&app, json!({
        "action": "update",
        "model": "User",
        "where": { "__id": user_id.clone() },
        "data": {
            "profile": { "set": null }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);
    
    // Ensure Profile is not deleted, just disconnected
    let c: i64 = conn.interact({
        let prof_id = prof_id.clone();
        move |db| db.query_row("SELECT COUNT(*) FROM Profile WHERE __id = ? AND userId IS NULL", [&prof_id], |r| r.get(0)).unwrap()
    }).await.unwrap();
    assert_eq!(c, 1);

    // Re-connect
    post_query(&app, json!({ "action": "update", "model": "User", "where": { "__id": user_id.clone() }, "data": { "profile": { "set": { "__id": prof_id.clone() } } } })).await;

    // 2. Cascading Delete (deleteDropped: true)
    let (status, _) = post_query(&app, json!({
        "action": "update",
        "model": "User",
        "where": { "__id": user_id.clone() },
        "data": {
            "profile": { "set": null, "deleteDropped": true }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // Ensure Profile is DELETED
    let c: i64 = conn.interact({
        let prof_id = prof_id.clone();
        move |db| db.query_row("SELECT COUNT(*) FROM Profile WHERE __id = ?", [&prof_id], |r| r.get(0)).unwrap()
    }).await.unwrap();
    assert_eq!(c, 0);

    // 3. Schema-Level onDisconnect: Delete
    let (status, _) = post_query(&app, json!({
        "action": "update",
        "model": "User",
        "where": { "__id": user_id.clone() },
        "data": {
            "avatar": { "set": null }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // Ensure Avatar is DELETED automatically
    let c: i64 = conn.interact({
        let avatar_id = avatar_id.clone();
        move |db| db.query_row("SELECT COUNT(*) FROM Avatar WHERE __id = ?", [&avatar_id], |r| r.get(0)).unwrap()
    }).await.unwrap();
    assert_eq!(c, 0);
}

#[tokio::test]
async fn test_singular_set_on_disconnect_restrict() {
    let schema = r#"
        model Manager {
            name: String
            worker: Worker? @relation("ManagerWorker", onDisconnect: Restrict, owner: true)
            @@id(uuid)
        }
        model Worker {
            name: String
            manager: Manager? @relation("ManagerWorker")
            @@id(uuid)
        }
    "#;
    let (app, pool, _dir) = setup_app_with_schema(schema).await;

    // Create Manager with Worker
    let (status, res) = post_query(&app, json!({
        "action": "create",
        "model": "Manager",
        "data": {
            "name": "Boss",
            "worker": { "create": { "name": "Employee" } }
        }
    })).await;
    println!("RES: {:?}", res);
    assert_eq!(status, StatusCode::OK);
    let manager_id = res["data"]["__id"].as_str().unwrap().to_string();

    let conn = pool.get().await.unwrap();
    let worker_id: String = conn.interact({
        let manager_id = manager_id.clone();
        move |db| {
            let mut stmt = db.prepare("SELECT __id, name, workerId FROM Manager").unwrap();
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0).unwrap();
                let name: String = row.get(1).unwrap();
                let w_id: Option<String> = row.get(2).unwrap();
                Ok((id, name, w_id))
            }).unwrap();
            for r in rows {
                println!("MANAGER ROW: {:?}", r.unwrap());
            }
            db.query_row("SELECT workerId FROM Manager WHERE __id = ?", [&manager_id], |r| r.get(0)).unwrap()
        }
    }).await.unwrap();

    // 1. Restricting set: null
    let (status, res) = post_query(&app, json!({
        "action": "update",
        "model": "Manager",
        "where": { "__id": manager_id.clone() },
        "data": {
            "worker": { "set": null }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: Cannot use 'set' on field 'worker' because it implies disconnecting existing relations, which is restricted by onDisconnect: Restrict."));

    // 2. Restricting implicit disconnects (set to a new worker)
    // Wait: our implementation currently blocks 'set' ENTIRELY if onDisconnect: Restrict. Let's verify that's the behavior we assert.
    let (status, res) = post_query(&app, json!({
        "action": "update",
        "model": "Manager",
        "where": { "__id": manager_id.clone() },
        "data": {
            "worker": { "set": { "__id": "some_other_id" } }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: Cannot use 'set' on field 'worker' because it implies disconnecting existing relations, which is restricted by onDisconnect: Restrict."));
}

#[tokio::test]
async fn test_singular_set_fk_directionality() {
    let schema = r#"
        model Parent {
            name: String
            childHoldsFk: ChildA?
            
            parentHoldsFkId: String?
            parentHoldsFk: ChildB?
            @@id(uuid)
        }
        model ChildA {
            name: String
            parentId: String? @unique
            parent: Parent?
            @@id(uuid)
        }
        model ChildB {
            name: String
            @@id(uuid)
        }
    "#;
    let (app, pool, _dir) = setup_app_with_schema(schema).await;

    // Create Children
    let (s, r) = post_query(&app, json!({ "action": "create", "model": "ChildA", "data": { "name": "A" } })).await;
    let child_a_id = r["data"]["__id"].as_str().unwrap().to_string();
    let (s, r) = post_query(&app, json!({ "action": "create", "model": "ChildB", "data": { "name": "B" } })).await;
    let child_b_id = r["data"]["__id"].as_str().unwrap().to_string();

    // Create Parent and connect to both
    let (status, res) = post_query(&app, json!({
        "action": "create",
        "model": "Parent",
        "data": {
            "name": "P1",
            "childHoldsFk": { "set": { "__id": child_a_id.clone() } },
            "parentHoldsFk": { "set": { "__id": child_b_id.clone() } }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);
    let parent_id = res["data"]["__id"].as_str().unwrap().to_string();

    let conn = pool.get().await.unwrap();

    // Verify ChildA holds FK
    let fk_a: String = conn.interact({ let c = child_a_id.clone(); move |db| db.query_row("SELECT parentId FROM ChildA WHERE __id = ?", [c], |r| r.get(0)).unwrap() }).await.unwrap();
    assert_eq!(fk_a, parent_id);

    // Verify Parent holds FK for ChildB
    let fk_b: String = conn.interact({ let p = parent_id.clone(); move |db| db.query_row("SELECT parentHoldsFkId FROM Parent WHERE __id = ?", [p], |r| r.get(0)).unwrap() }).await.unwrap();
    assert_eq!(fk_b, child_b_id);

    // Disconnect both
    let (status, _) = post_query(&app, json!({
        "action": "update",
        "model": "Parent",
        "where": { "__id": parent_id.clone() },
        "data": {
            "childHoldsFk": { "set": null },
            "parentHoldsFk": { "set": null }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    // Verify disconnected
    let fk_a_null: Option<String> = conn.interact({ let c = child_a_id.clone(); move |db| db.query_row("SELECT parentId FROM ChildA WHERE __id = ?", [c], |r| r.get(0)).unwrap() }).await.unwrap();
    assert!(fk_a_null.is_none());

    let fk_b_null: Option<String> = conn.interact({ let p = parent_id.clone(); move |db| db.query_row("SELECT parentHoldsFkId FROM Parent WHERE __id = ?", [p], |r| r.get(0)).unwrap() }).await.unwrap();
    assert!(fk_b_null.is_none());
}

#[tokio::test]
async fn test_singular_set_bulk_mutation_rejections() {
    let schema = r#"
        model Team {
            name: String
            leader: Leader?
            @@id(uuid)
        }
        model Leader {
            name: String
            teamId: String? @unique
            team: Team?
            @@id(uuid)
        }
    "#;
    let (app, _, _dir) = setup_app_with_schema(schema).await;

    // 1. Blocked Set in updateMany
    let (status, res) = post_query(&app, json!({
        "action": "updateMany",
        "model": "Team",
        "where": { "name": "Devs" },
        "data": {
            "leader": { "set": { "__id": "leader1" } }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: Cannot 'set' a relation to multiple distinct parents in a bulk update."));

    // 2. Blocked Nullification in updateMany
    let (status, res) = post_query(&app, json!({
        "action": "updateMany",
        "model": "Team",
        "where": { "name": "Devs" },
        "data": {
            "leader": { "set": null }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Semantics Error: Cannot 'set' a relation to multiple distinct parents in a bulk update."));
}

#[tokio::test]
async fn test_singular_polymorphic_unconditional_nullify_and_legacy_rejection() {
    let schema = r#"
        base Content {}
        model Video extends Content { duration: Int @@id(uuid) }
        model Article extends Content { words: Int @@id(uuid) }
        
        model Bookmark {
            name: String
            contentId: String?
            contentType: String?
            content: Content?
            @@id(uuid)
        }
    "#;
    let (app, pool, _dir) = setup_app_with_schema(schema).await;

    let (s, r) = post_query(&app, json!({ "action": "create", "model": "Video", "data": { "duration": 120 } })).await;
    let vid_id = r["data"]["__id"].as_str().unwrap().to_string();

    let (s, r) = post_query(&app, json!({
        "action": "create",
        "model": "Bookmark",
        "data": {
            "name": "My Fav",
            "content": { "set": { "__kind": "Video", "__id": vid_id.clone() } }
        }
    })).await;
    assert_eq!(s, StatusCode::OK);
    let bk_id = r["data"]["__id"].as_str().unwrap().to_string();

    // 1. Reject conditional payloads (legacy disconnect format incorrectly placed in set)
    let (status, res) = post_query(&app, json!({
        "action": "update",
        "model": "Bookmark",
        "where": { "__id": bk_id.clone() },
        "data": {
            "content": { "set": { "where": { "__kind": "Video" } } }
        }
    })).await;
    assert_ne!(status, StatusCode::OK);
    assert!(res["error"].as_str().unwrap().contains("Polymorphic set requires '__kind'"), "Should fail because it's not a valid set payload");

    // 2. Unconditional Nullify (set: null)
    let (status, _) = post_query(&app, json!({
        "action": "update",
        "model": "Bookmark",
        "where": { "__id": bk_id.clone() },
        "data": {
            "content": { "set": null }
        }
    })).await;
    assert_eq!(status, StatusCode::OK);

    let conn = pool.get().await.unwrap();
    let (c_id, c_type): (Option<String>, Option<String>) = conn.interact({ let b = bk_id.clone(); move |db| {
        db.query_row("SELECT contentId, contentType FROM Bookmark WHERE __id = ?", [b], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap()))).unwrap()
    }}).await.unwrap();
    
    assert!(c_id.is_none());
    assert!(c_type.is_none());
}
