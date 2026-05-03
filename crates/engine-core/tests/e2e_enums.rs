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
        enum Role {
            ADMIN
            USER
            GUEST
        }

        model Account {
            role: Role
            roles: Role[]
            @@id(uuid)
        }
    "#;
    
    fs::write(workspace.join("schema.cq"), schema).unwrap();
    
    let mut caqui_migrate = Command::new(caqui_bin);
    caqui_migrate.arg("schema").arg("migrate").current_dir(workspace);
    run_cmd(caqui_migrate);

    let schema_str = fs::read_to_string(workspace.join("schema.cq")).unwrap();
    let ast = schema_parser::parse_schema(&schema_str).unwrap();
    let ast = schema_parser::validate_schema(ast).unwrap();
    
    let db_path = workspace.join("app.db");
    let db_uri = format!("file:{}?vfs=git", db_path.display());
    let pool = api_layer::db::create_pool(&db_uri);
    
    let state = api_layer::state::EngineState {
        ast: Arc::new(ast),
        db_pool: pool,
    };
    let app = api_layer::router::build_dynamic_router(state);

    (app, dir)
}

#[tokio::test]
async fn test_valid_enum_mutations() {
    let (app, _dir) = setup_app().await;

    // 1. Create
    let create_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "create",
            "data": {
                "role": "ADMIN",
                "roles": ["USER", "GUEST"]
            },
            "select": { "__id": true, "role": true, "roles": true }
        }).to_string()))
        .unwrap();

    let res = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let data = &json["data"];
    assert_eq!(data["role"], "ADMIN");
    assert_eq!(data["roles"], json!(["USER", "GUEST"]));
    let id = data["__id"].as_str().unwrap().to_string();

    // 2. Update
    let update_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "update",
            "where": { "__id": id },
            "data": {
                "role": "USER",
                "roles": { "push": "ADMIN" }
            },
            "select": { "role": true, "roles": true }
        }).to_string()))
        .unwrap();

    let res2 = app.clone().oneshot(update_req).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let body2 = axum::body::to_bytes(res2.into_body(), 1024 * 1024).await.unwrap();
    let json2: Value = serde_json::from_slice(&body2).unwrap();
    assert_eq!(json2["data"]["role"], "USER");
    // SQLite JSON1 appends; result should be ["USER", "GUEST", "ADMIN"]
    assert_eq!(json2["data"]["roles"], json!(["USER", "GUEST", "ADMIN"]));
}

#[tokio::test]
async fn test_invalid_enum_mutations() {
    let (app, _dir) = setup_app().await;

    // 1. Invalid variant
    let req1 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "create",
            "data": { "role": "SUPERADMIN" },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(res1.status(), StatusCode::BAD_REQUEST);

    // 2. Invalid variant in array
    let req2 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "create",
            "data": { "role": "ADMIN", "roles": ["ADMIN", "HACKER"] },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);

    // 3. Wrong type
    let req3 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "create",
            "data": { "role": 123 },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res3 = app.clone().oneshot(req3).await.unwrap();
    assert_eq!(res3.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_enum_filtering() {
    let (app, _dir) = setup_app().await;

    let roles = vec!["ADMIN", "USER", "GUEST"];
    for r in roles {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({
                "model": "Account",
                "action": "create",
                "data": { "role": r },
                "select": { "__id": true }
            }).to_string()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap();
    }

    // 1. Exact Match
    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "findMany",
            "where": { "role": "ADMIN" },
            "select": { "role": true }
        }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(query_req).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["data"].as_array().unwrap().len(), 1);

    // 2. In Array
    let query_in = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "findMany",
            "where": { "role": { "in": ["ADMIN", "GUEST"] } },
            "select": { "role": true }
        }).to_string()))
        .unwrap();
    let res_in = app.clone().oneshot(query_in).await.unwrap();
    let body_in = axum::body::to_bytes(res_in.into_body(), 1024 * 1024).await.unwrap();
    let json_in: Value = serde_json::from_slice(&body_in).unwrap();
    assert_eq!(json_in["data"].as_array().unwrap().len(), 2);

    // 3. Invalid Filter Variant
    let query_invalid = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "model": "Account",
            "action": "findMany",
            "where": { "role": "UNKNOWN" },
            "select": { "role": true }
        }).to_string()))
        .unwrap();
    let res_invalid = app.clone().oneshot(query_invalid).await.unwrap();
    assert_eq!(res_invalid.status(), StatusCode::BAD_REQUEST);
}