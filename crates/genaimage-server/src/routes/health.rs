use axum::{extract::State, routing::get, Json, Router};
use genaimage_api::{HealthResponse, HealthStatus};

use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let image_backend = match state.harness.health().await {
        Ok(_) => HealthStatus::Ok,
        Err(_) => HealthStatus::Unavailable,
    };
    Json(HealthResponse {
        server: HealthStatus::Ok,
        image_backend,
        llm: HealthStatus::NotConfigured,
    })
}
