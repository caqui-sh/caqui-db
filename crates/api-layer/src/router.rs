use axum::{routing::post, Router};
use crate::state::EngineState;

pub fn build_dynamic_router(state: EngineState) -> Router {
    Router::new()
        // The universal dynamic endpoint catching all read/write API requests
        .route("/api/v1/query", post(crate::handler::api_execution_handler))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    use deadpool_sqlite::{Config, Runtime};
    use std::sync::Arc;
    use schema_parser::ast::SchemaAst;
    use std::collections::HashMap;

    async fn mock_state() -> EngineState {
        let pool = Config::new("file::memory:?cache=shared")
            .create_pool(Runtime::Tokio1)
            .unwrap();

        EngineState {
            ast: Arc::new(SchemaAst {
                models: HashMap::new(),
                unions: HashMap::new(),
            }),
            db_pool: pool,
        }
    }

    #[tokio::test]
    async fn test_router_wiring_post() {
        let state = mock_state().await;
        let app = build_dynamic_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/query")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model": "User", "action": "findMany"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // It might be 400 or 501 depending on AST, but it shouldn't be 404
        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_router_wiring_get() {
        let state = mock_state().await;
        let app = build_dynamic_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/query")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // GET is not allowed on this route
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
