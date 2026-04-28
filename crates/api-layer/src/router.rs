use axum::{routing::post, Router};
use crate::state::EngineState;

pub fn build_dynamic_router(state: EngineState) -> Router {
    Router::new()
        // The universal dynamic endpoint catching all read/write API requests
        .route("/api/v1/query", post(crate::handler::api_execution_handler))
        .with_state(state)
}
