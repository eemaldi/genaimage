use std::time::Duration;

use axum::{extract::State, Json};
use genaimage_api::{
    HealthHints, HealthResponse, HealthStatus, ImageBackendHealthReason,
    LlmRoleHealth as ApiLlmRoleHealth,
};
use genaimage_core::{LlmRoleHealth, LlmRoleStatus};
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
    state.llm_roles().refresh_in_background();
    let llm_snapshot = state.llm_roles().health();
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
        llm: health_status(llm_snapshot.status),
        llm_reason: llm_snapshot.reason,
        llm_text: llm_snapshot.text.map(role_health),
        llm_vision: llm_snapshot.vision.map(role_health),
        hints: Some(HealthHints {
            server: "Start the local GenAImage server and check its configuration.".into(),
            image_backend: "Start ComfyUI and check `comfyui.base_url` in Settings or configuration; restart GenAImage after changing the endpoint.".into(),
            llm: "Set llm.base_url plus llm.text_model and optionally llm.vision_model, then use Test LLM connection in Settings and restart after saving.".into(),
        }),
    })
}

fn health_status(status: LlmRoleStatus) -> HealthStatus {
    match status {
        LlmRoleStatus::Ok => HealthStatus::Ok,
        LlmRoleStatus::NotConfigured => HealthStatus::NotConfigured,
        LlmRoleStatus::Unavailable => HealthStatus::Unavailable,
    }
}

fn role_health(role: LlmRoleHealth) -> ApiLlmRoleHealth {
    ApiLlmRoleHealth {
        status: health_status(role.status),
        model: role.model,
        reason: role.reason,
    }
}
