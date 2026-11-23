use std::time::Duration;

use axum::{extract::State, routing::get, Json, Router};
use genaimage_api::{HealthResponse, HealthStatus, ImageBackendHealthReason};
use tokio::time::timeout;

use crate::AppState;

const PROBE_DEADLINE: Duration = Duration::from_secs(2);

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let image_backend_reason = match timeout(PROBE_DEADLINE, state.harness.health()).await {
        Ok(Ok(_)) => None,
        Ok(Err(_)) => Some(ImageBackendHealthReason::ProbeFailed),
        Err(_) => Some(ImageBackendHealthReason::Timeout),
    };
    Json(HealthResponse {
        server: HealthStatus::Ok,
        image_backend: if image_backend_reason.is_some() {
            HealthStatus::Unavailable
        } else {
            HealthStatus::Ok
        },
        image_backend_reason,
        llm: HealthStatus::NotConfigured,
    })
}
