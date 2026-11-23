use std::time::Duration;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderName},
    routing::{get, post},
    Json, Router,
};
use genaimage_api::{
    HealthStatus, ImageBackendHealthReason, SettingsPatch, SettingsResponse, SettingsTestResponse,
};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    config::{validate_endpoint, validate_endpoint_addresses, ComfyUiProfile},
    image_backend::ImageBackend,
};
use tokio::time::timeout;

use crate::{settings::save_failed, AppState, Problem};

const PROBE_DEADLINE: Duration = Duration::from_secs(2);
const NO_STORE: [(HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];
type SettingsReply = ([(HeaderName, &'static str); 1], Json<SettingsResponse>);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings", get(read).put(save))
        .route("/settings/test", post(probe))
}

#[utoipa::path(
    get, path = "/api/v1/settings",
    operation_id = "settings_read",
    description = "Read effective settings.",
    responses(
        (status = 200, body = genaimage_api::SettingsResponse, headers(("Cache-Control" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn read(State(state): State<AppState>) -> SettingsReply {
    (NO_STORE, Json(state.settings.get()))
}

#[utoipa::path(
    put, path = "/api/v1/settings",
    operation_id = "settings_save",
    description = "Save sparse settings.",
    request_body = genaimage_api::SettingsPatch,
    responses(
        (status = 200, body = genaimage_api::SettingsResponse, headers(("Cache-Control" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Settings conflict", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn save(
    State(state): State<AppState>,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<SettingsReply, Problem> {
    let patch = parse(patch)?;
    let saved = tokio::task::spawn_blocking(move || state.settings.save(patch))
        .await
        .map_err(|_| save_failed())??;
    Ok((NO_STORE, Json(saved)))
}

#[utoipa::path(
    post, path = "/api/v1/settings/test",
    operation_id = "settings_probe",
    description = "Test proposed settings without saving them.",
    request_body = genaimage_api::SettingsPatch,
    responses(
        (status = 200, body = genaimage_api::SettingsTestResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn probe(
    State(state): State<AppState>,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<Json<SettingsTestResponse>, Problem> {
    let proposal = state.settings.propose(parse(patch)?, true)?;
    let config = proposal.config();
    let (reason, llm) = tokio::join!(
        probe_comfyui(&config.comfyui),
        probe_llm(&config.llm.base_url)
    );
    Ok(Json(SettingsTestResponse {
        comfyui: if reason.is_none() {
            HealthStatus::Ok
        } else {
            HealthStatus::Unavailable
        },
        comfyui_reason: reason,
        llm,
    }))
}

async fn probe_comfyui(profile: &ComfyUiProfile) -> Option<ImageBackendHealthReason> {
    // Use the normal transport: pinned validated addresses, no proxy or redirects.
    let Ok(client) = ComfyUiClient::new(profile) else {
        return Some(ImageBackendHealthReason::ProbeFailed);
    };
    match timeout(PROBE_DEADLINE, client.health()).await {
        Ok(Ok(_)) => None,
        Ok(Err(_)) => Some(ImageBackendHealthReason::ProbeFailed),
        Err(_) => Some(ImageBackendHealthReason::Timeout),
    }
}

async fn probe_llm(base_url: &str) -> HealthStatus {
    if base_url.is_empty() {
        return HealthStatus::NotConfigured;
    }
    let result = timeout(PROBE_DEADLINE, async {
        let key = "llm.base_url";
        let mut url = validate_endpoint(key, base_url, &[]).ok()?;
        let host = url.host_str()?.trim_matches(['[', ']']).to_owned();
        let addresses: Vec<_> =
            tokio::net::lookup_host((host.as_str(), url.port_or_known_default()?))
                .await
                .ok()?
                .collect();
        validate_endpoint_addresses(key, base_url, &[], &addresses).ok()?;
        // OpenAI-compatible roots normally end in /v1. Honour explicit base paths.
        if url.path() == "/" {
            url.set_path("/v1/models");
        } else {
            url.path_segments_mut().ok()?.pop_if_empty().push("models");
        }
        let response = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addresses)
            .build()
            .ok()?
            .get(url)
            .send()
            .await
            .ok()?;
        Some(response.status().is_success())
    })
    .await;
    if matches!(result, Ok(Some(true))) {
        HealthStatus::Ok
    } else {
        HealthStatus::Unavailable
    }
}

fn parse(input: Result<Json<SettingsPatch>, JsonRejection>) -> Result<SettingsPatch, Problem> {
    input.map(|Json(patch)| patch).map_err(|error| {
        Problem::new(
            error.status(),
            "The settings request is not a JSON object.",
            "Send an application/json object with dotted setting keys.",
        )
    })
}
