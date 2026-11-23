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
use genaimage_core::image_backend::ImageBackend;
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

async fn read(State(state): State<AppState>) -> SettingsReply {
    (NO_STORE, Json(state.settings.get()))
}

async fn save(
    State(state): State<AppState>,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<SettingsReply, Problem> {
    let patch = parse(patch)?;
    let saved = tokio::task::spawn_blocking(move || state.settings.save(patch))
        .await
        .map_err(|_| save_failed())??;
    Ok((NO_STORE, Json(saved)))
}

async fn probe(
    State(state): State<AppState>,
    patch: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<Json<SettingsTestResponse>, Problem> {
    let proposal = state.settings.propose(parse(patch)?, true)?;
    // Use the normal transport: pinned validated addresses, no proxy or redirects.
    let reason = match ComfyUiClient::new(&proposal.config().comfyui) {
        Ok(client) => match timeout(PROBE_DEADLINE, client.health()).await {
            Ok(Ok(_)) => None,
            Ok(Err(_)) => Some(ImageBackendHealthReason::ProbeFailed),
            Err(_) => Some(ImageBackendHealthReason::Timeout),
        },
        Err(_) => Some(ImageBackendHealthReason::ProbeFailed),
    };
    Ok(Json(SettingsTestResponse {
        comfyui: if reason.is_none() {
            HealthStatus::Ok
        } else {
            HealthStatus::Unavailable
        },
        comfyui_reason: reason,
        llm: HealthStatus::NotConfigured,
    }))
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
