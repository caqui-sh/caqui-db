use axum::{extract::State, extract::Json, response::IntoResponse, http::StatusCode, http::header};
use serde_json::Value;
use crate::state::EngineState;
use crate::translator::hydrate_payload_to_ir;

pub async fn api_execution_handler(
    State(state): State<EngineState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    
    let model = payload["model"].as_str().unwrap_or_default();
    let action = payload["action"].as_str().unwrap_or_default();

    if action == "findMany" {
        let mut alias_idx = 0;
        
        // 1. Validate & Hydrate to IR (Phase 5)
        let query_ir = match hydrate_payload_to_ir(&state.ast, model, &payload, &mut alias_idx) {
            Ok(ir) => ir,
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        };

        // 2. Compile IR to a single JSON-aggregating SQL string (Phase 4)
        let sql_query = query_compiler::read::compile_select(&query_ir, None);

        // 3. Thread-safe execution against the custom VFS-backed SQLite pool
        let conn = state.db_pool.get().await.unwrap();
        
        // `interact` pushes the blocking SQLite C-FFI call to a dedicated thread, 
        // preventing Tokio async worker starvation.
        let raw_json_string = conn.interact(move |db| {
            db.prepare_cached(&sql_query)
                .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)))
        }).await;

        match raw_json_string {
            Ok(Ok(json_payload)) => {
                // 4. Zero-Overhead HTTP Proxying: Do not deserialize into Rust structs!
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    format!("{{\"data\": {}}}", json_payload), 
                ).into_response()
            },
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Database Error: {}", e)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Execution Error: {}", e)).into_response(),
        }
    } else {
        (StatusCode::NOT_IMPLEMENTED, "Mutation logic").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
        routing::post,
    };
    use tower::ServiceExt; // for `call`, `into_service`, and `oneshot`
    use deadpool_sqlite::{Config, Runtime};
    use std::sync::Arc;
    use schema_parser::ast::{SchemaAst, ModelNode, FieldNode, AstFieldType};
    use std::collections::HashMap;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

    async fn build_test_state() -> EngineState {
        // Setup an AST
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
            ]
        });

        // Setup the Deadpool sqlite using a unique shared cache name for isolation between tests
        let db_name = format!("file:memdb{}?mode=memory&cache=shared", DB_COUNTER.fetch_add(1, Ordering::SeqCst));
        let pool = Config::new(db_name)
            .create_pool(Runtime::Tokio1)
            .unwrap();

        // Seed DB directly through the pool
        let conn = pool.get().await.unwrap();
        conn.interact(|db| -> Result<(), rusqlite::Error> {
            db.execute("CREATE TABLE User (id TEXT PRIMARY KEY, name TEXT);", [])?;
            db.execute("INSERT INTO User (id, name) VALUES ('u1', 'Bob');", [])?;
            Ok(())
        }).await.unwrap().unwrap();

        EngineState {
            ast: Arc::new(ast),
            db_pool: pool,
        }
    }

    #[tokio::test]
    async fn test_api_execution_handler_valid() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "select": {
                        "id": true,
                        "name": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("\"id\":\"u1\""));
        assert!(body_str.contains("\"name\":\"Bob\""));
    }

    #[tokio::test]
    async fn test_api_execution_handler_bad_request() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "select": {
                        "unknown_field": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Invalid field 'unknown_field'"));
    }

    #[tokio::test]
    async fn test_api_execution_handler_unsupported_action() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "createMany",
                    "data": []
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_api_execution_handler_db_error() {
        let mut state = build_test_state().await;
        
        // Add a model to AST that DOES NOT have a table in DB
        let mut ast = (*state.ast).clone();
        ast.models.insert("Ghost".to_string(), ModelNode {
            name: "Ghost".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), attributes: vec![] },
            ]
        });
        state.ast = Arc::new(ast);

        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Ghost",
                    "action": "findMany",
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should fail because table 'Ghost' doesn't exist
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_api_execution_handler_missing_keys() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        // Completely empty payload
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Missing action defaults to "" which hits NOT_IMPLEMENTED
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
