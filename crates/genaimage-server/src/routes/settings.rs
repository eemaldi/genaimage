use std::time::Duration;

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::{header, HeaderMap, HeaderName, StatusCode},
    Json,
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
type SettingsReply = ([(HeaderName, String); 2], Json<SettingsResponse>);
fn reply((etag, response): (String, SettingsResponse)) -> SettingsReply {
    (
        [
            (header::CACHE_CONTROL, "no-store".into()),
            (header::ETAG, etag),
        ],
        Json(response),
    )
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ProbeQuery {
    /// Choose one service; omit to test both for backwards compatibility.
    service: Option<String>,
}

#[utoipa::path(
    get, path = "/settings",
    operation_id = "settings_read",
    tag = "settings",
    description = "Read effective settings.",
    responses(
        (status = 200, body = genaimage_api::SettingsResponse, headers(("Cache-Control" = String), ("ETag" = String))),
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
    reply(state.settings.get())
}

#[utoipa::path(
    put, path = "/settings",
    operation_id = "settings_save",
    tag = "settings",
    description = "Save sparse settings. If-Match is optional for older clients; use the GET ETag to prevent lost updates.",
    params(("If-Match" = Option<String>, Header, description = "Current settings ETag")),
    request_body = genaimage_api::SettingsPatch,
    responses(
        (status = 200, body = genaimage_api::SettingsResponse, headers(("Cache-Control" = String), ("ETag" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 412, description = "Settings revision mismatch", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
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
    headers: HeaderMap,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<SettingsReply, Problem> {
    let patch = parse(patch)?;
    let if_match = parse_if_match(&headers)?;
    let saved = tokio::task::spawn_blocking(move || state.settings.save(patch, if_match))
        .await
        .map_err(|_| save_failed())??;
    Ok(reply(saved))
}

#[utoipa::path(
    post, path = "/settings/test",
    operation_id = "settings_probe",
    tag = "settings",
    description = "Test proposed settings without saving them. If-Match is optional for older clients; use the GET ETag to test the loaded revision.",
    request_body = genaimage_api::SettingsPatch,
    params(ProbeQuery, ("If-Match" = Option<String>, Header, description = "Current settings ETag")),
    responses(
        (status = 200, body = genaimage_api::SettingsTestResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 412, description = "Settings revision mismatch", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
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
    query: Result<Query<ProbeQuery>, QueryRejection>,
    headers: HeaderMap,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<Json<SettingsTestResponse>, Problem> {
    let Query(query) = query.map_err(|_| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid test service.",
            "Choose comfyui or llm, or omit service to test both.",
        )
    })?;
    let service = match query.service.as_deref() {
        Some("comfyui") => "comfyui",
        Some("llm") => "llm",
        None => "both",
        _ => {
            return Err(Problem::new(
                StatusCode::BAD_REQUEST,
                "Invalid test service.",
                "Choose comfyui or llm, or omit service to test both.",
            ))
        }
    };
    let if_match = parse_if_match(&headers)?;
    let proposal = state
        .settings
        .propose(parse(patch)?, Some(service), if_match.as_deref())?;
    let config = proposal.config();
    let (reason, llm) = tokio::join!(
        async {
            if service != "llm" {
                probe_comfyui(&config.comfyui).await
            } else {
                None
            }
        },
        async {
            if service != "comfyui" {
                probe_llm(&config.llm.base_url).await
            } else {
                HealthStatus::NotConfigured
            }
        }
    );
    Ok(Json(SettingsTestResponse {
        comfyui: if service == "llm" {
            HealthStatus::NotConfigured
        } else if reason.is_none() {
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

fn parse_if_match(headers: &HeaderMap) -> Result<Option<String>, Problem> {
    let if_match = headers
        .get_all(header::IF_MATCH)
        .iter()
        .map(|value| value.to_str().map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                "Invalid If-Match header.",
                "Use the ETag returned by GET settings.",
            )
        })?;
    Ok((!if_match.is_empty()).then(|| if_match.join(",")))
}
