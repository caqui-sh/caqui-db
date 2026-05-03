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
        model Reading {
            value: Float
            recordedAt: DateTime
            optionalValue: Float?
            updatedAt: DateTime?
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
async fn test_float_precision_and_filtering() {
    let (app, _dir) = setup_app().await;

    let values = vec![10.5, 10.05, 10.55];
    for v in values {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "model": "Reading",
                "action": "create",
                "data": {
                    "value": v,
                    "recordedAt": "2025-01-01T00:00:00Z"
                },
                "select": { "value": true }
            }).to_string()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert_eq!(status, StatusCode::OK, "Body: {}", body_str);
    }

    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "where": {
                "value": { "gte": 10.1, "lte": 10.5 }
            },
            "select": { "value": true }
        }).to_string()))
        .unwrap();

    let res = app.clone().oneshot(query_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    
    let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    let records = json.get("data").unwrap().as_array().unwrap();
    
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].get("value").unwrap().as_f64().unwrap(), 10.5);

    let query_req2 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "orderBy": {
                "value": "desc"
            },
            "select": { "value": true }
        }).to_string()))
        .unwrap();

    let res2 = app.clone().oneshot(query_req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    
    let body_bytes2 = axum::body::to_bytes(res2.into_body(), 1024 * 1024).await.unwrap();
    let json2: Value = serde_json::from_slice(&body_bytes2).unwrap();
    let records2 = json2.get("data").unwrap().as_array().unwrap();
    
    assert_eq!(records2.len(), 3);
    assert_eq!(records2[0].get("value").unwrap().as_f64().unwrap(), 10.55);
    assert_eq!(records2[1].get("value").unwrap().as_f64().unwrap(), 10.5);
    assert_eq!(records2[2].get("value").unwrap().as_f64().unwrap(), 10.05);
}

#[tokio::test]
async fn test_datetime_utc_normalization() {
    let (app, _dir) = setup_app().await;

    let req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "create",
            "data": {
                "value": 42.0,
                "recordedAt": "2025-10-10T12:00:00-04:00"
            },
            "select": { "recordedAt": true }
        }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "select": { "recordedAt": true }
        }).to_string()))
        .unwrap();

    let res2 = app.clone().oneshot(query_req).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    
    let body_bytes = axum::body::to_bytes(res2.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    let records = json.get("data").unwrap().as_array().unwrap();
    let record = &records[0];
    
    assert_eq!(record.get("recordedAt").unwrap().as_str().unwrap(), "2025-10-10T16:00:00.000Z");
}

#[tokio::test]
async fn test_temporal_filtering_and_sorting() {
    let (app, _dir) = setup_app().await;

    let dates = vec![
        "2023-01-01T00:00:00Z",
        "2024-06-15T12:30:45.500Z",
        "2024-06-15T12:30:45.501Z"
    ];
    for d in dates {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "model": "Reading",
                "action": "create",
                "data": {
                    "value": 1.0,
                    "recordedAt": d
                },
                "select": { "recordedAt": true }
            }).to_string()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert_eq!(status, StatusCode::OK, "Body: {}", body_str);
    }

    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "where": {
                "recordedAt": {
                    "gte": "2024-01-01T00:00:00Z",
                    "lt": "2025-01-01T00:00:00Z"
                }
            },
            "select": { "recordedAt": true }
        }).to_string()))
        .unwrap();

    let res = app.clone().oneshot(query_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    
    let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    let records = json.get("data").unwrap().as_array().unwrap();
    
    assert_eq!(records.len(), 2);

    let query_req2 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "orderBy": {
                "recordedAt": "asc"
            },
            "select": { "recordedAt": true }
        }).to_string()))
        .unwrap();

    let res2 = app.clone().oneshot(query_req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    
    let body_bytes2 = axum::body::to_bytes(res2.into_body(), 1024 * 1024).await.unwrap();
    let json2: Value = serde_json::from_slice(&body_bytes2).unwrap();
    let records2 = json2.get("data").unwrap().as_array().unwrap();
    
    assert_eq!(records2.len(), 3);
    assert_eq!(records2[0].get("recordedAt").unwrap().as_str().unwrap(), "2023-01-01T00:00:00.000Z");
    assert_eq!(records2[1].get("recordedAt").unwrap().as_str().unwrap(), "2024-06-15T12:30:45.500Z");
    assert_eq!(records2[2].get("recordedAt").unwrap().as_str().unwrap(), "2024-06-15T12:30:45.501Z");
}

#[tokio::test]
async fn test_validation_rejections() {
    let (app, _dir) = setup_app().await;

    let req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "create",
            "data": {
                "value": 1.0,
                "recordedAt": "Next Tuesday"
            },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let req2 = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "create",
            "data": {
                "value": "100.5",
                "recordedAt": "2025-01-01T00:00:00Z"
            },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_mutation_lifecycle_update() {
    let (app, _dir) = setup_app().await;

    // 1. Create
    let create_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "create",
            "data": {
                "value": 1.0,
                "recordedAt": "2025-01-01T00:00:00Z"
            },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    let id = json["data"]["__id"].as_str().unwrap();

    // 2. Update with new offset and value
    let update_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "update",
            "where": { "__id": id },
            "data": {
                "value": 2.5,
                "recordedAt": "2025-10-10T12:00:00-04:00"
            },
            "select": { "value": true, "recordedAt": true }
        }).to_string()))
        .unwrap();
    let res2 = app.clone().oneshot(update_req).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let body_bytes2 = axum::body::to_bytes(res2.into_body(), 1024 * 1024).await.unwrap();
    let json2: Value = serde_json::from_slice(&body_bytes2).unwrap();
    let record = &json2["data"];

    assert_eq!(record["value"].as_f64().unwrap(), 2.5);
    assert_eq!(record["recordedAt"].as_str().unwrap(), "2025-10-10T16:00:00.000Z");
}

#[tokio::test]
async fn test_broad_filter_operators() {
    let (app, _dir) = setup_app().await;

    let items = vec![
        ("2024-01-01T00:00:00Z", 10.0),
        ("2024-02-01T00:00:00Z", 20.0),
        ("2024-03-01T00:00:00Z", 30.0),
    ];

    for (d, v) in items {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "model": "Reading",
                "action": "create",
                "data": { "value": v, "recordedAt": d },
                "select": { "__id": true }
            }).to_string()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap();
    }

    // Test 'in' operator
    let query_in = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "where": {
                "recordedAt": { "in": ["2024-01-01T00:00:00Z", "2024-03-01T00:00:00Z"] }
            },
            "select": { "value": true }
        }).to_string()))
        .unwrap();
    let res_in = app.clone().oneshot(query_in).await.unwrap();
    let body_in = axum::body::to_bytes(res_in.into_body(), 1024 * 1024).await.unwrap();
    let json_in: Value = serde_json::from_slice(&body_in).unwrap();
    let records_in = json_in["data"].as_array().unwrap();
    assert_eq!(records_in.len(), 2);

    // Test 'notEq' operator
    let query_not_eq = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "where": {
                "value": { "notEq": 20.0 }
            },
            "select": { "value": true }
        }).to_string()))
        .unwrap();
    let res_not_eq = app.clone().oneshot(query_not_eq).await.unwrap();
    let body_not_eq = axum::body::to_bytes(res_not_eq.into_body(), 1024 * 1024).await.unwrap();
    let json_not_eq: Value = serde_json::from_slice(&body_not_eq).unwrap();
    let records_not_eq = json_not_eq["data"].as_array().unwrap();
    assert_eq!(records_not_eq.len(), 2);
}

#[tokio::test]
async fn test_edge_case_numeric_boundaries() {
    let (app, _dir) = setup_app().await;

    let values = vec![42.5, -42.5, 0.0];
    for v in values {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/api/v1/query")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({
                "model": "Reading",
                "action": "create",
                "data": { "value": v, "recordedAt": "2025-01-01T00:00:00Z" },
                "select": { "__id": true }
            }).to_string()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap();
    }

    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "orderBy": { "value": "asc" },
            "select": { "value": true }
        }).to_string()))
        .unwrap();

    let res = app.clone().oneshot(query_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    let records = json["data"].as_array().unwrap();

    assert_eq!(records[0]["value"].as_f64().unwrap(), -42.5);
    assert_eq!(records[1]["value"].as_f64().unwrap(), 0.0);
    assert_eq!(records[2]["value"].as_f64().unwrap(), 42.5);
}

#[tokio::test]
async fn test_nullability_and_omission() {
    let (app, _dir) = setup_app().await;

    let create_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "create",
            "data": {
                "value": 10.0,
                "recordedAt": "2025-01-01T00:00:00Z",
                "optionalValue": null,
                "updatedAt": null
            },
            "select": { "__id": true }
        }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let query_req = Request::builder()
        .method(http::Method::POST)
        .uri("/api/v1/query")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "model": "Reading",
            "action": "findMany",
            "where": {
                "updatedAt": { "isNull": true }
            },
            "select": { "value": true }
        }).to_string()))
        .unwrap();

    let res_query = app.clone().oneshot(query_req).await.unwrap();
    assert_eq!(res_query.status(), StatusCode::OK);
    let body_query = axum::body::to_bytes(res_query.into_body(), 1024 * 1024).await.unwrap();
    let json_query: Value = serde_json::from_slice(&body_query).unwrap();
    let records = json_query["data"].as_array().unwrap();
    assert_eq!(records.len(), 1);
}