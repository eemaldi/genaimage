use std::time::Duration;

use axum::{extract::State, Json};
use genaimage_api::{HealthResponse, HealthStatus, ImageBackendHealthReason};
use tokio::time::timeout;

use crate::AppState;

const PROBE_DEADLINE: Duration = Duration::from_secs(2);

#[utoipa::path(
    get, path = "/health",
    operation_id = "health_health",
    tag = "health",
    description = "Health of the local server and configured backends.",
    responses(
        (status = 200, body = genaimage_api::HealthResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let image_backend_reason = match timeout(PROBE_DEADLINE, state.harness().health()).await {
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
