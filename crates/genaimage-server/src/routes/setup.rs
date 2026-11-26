use axum::{extract::State, http::header, Json};
use genaimage_api::{SetupDetectResponse, SetupStatus};

use crate::{setup, AppState};

#[utoipa::path(
    get, path = "/setup/status",
    operation_id = "setup_status",
    tag = "setup",
    description = "Offline first-run assessment of the saved configuration. `needs_setup` is true when no configuration file exists and the environment does not supply the ComfyUI endpoint, or when the saved endpoints fail validation. Advisory reasons are non-blocking. No service is probed.",
    responses(
        (status = 200, body = genaimage_api::SetupStatus),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn status(
    State(state): State<AppState>,
) -> ([(header::HeaderName, &'static str); 1], Json<SetupStatus>) {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(setup::status(&state.settings.desired())),
    )
}

#[utoipa::path(
    post, path = "/setup/detect",
    operation_id = "setup_detect",
    tag = "setup",
    description = "Probe a fixed list of local ComfyUI and OpenAI-compatible LLM addresses plus the saved URLs, concurrently, with a 1.5 s deadline each. Takes no body and no user-supplied hosts. Hosts outside the allow-list are reported as skipped with the setting to add, never probed. The LLM API key is sent only to the saved LLM URL.",
    responses(
        (status = 200, body = genaimage_api::SetupDetectResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn detect(State(state): State<AppState>) -> Json<SetupDetectResponse> {
    let config = state.settings.desired();
    Json(SetupDetectResponse {
        services: setup::detect(
            setup::candidates(&config),
            &config.comfyui.allowed_hosts,
            &config.llm.allowed_hosts,
            config.llm.api_key.as_deref(),
        )
        .await,
    })
}
