//! Explicit model bootstrap. No handler here starts a download except
//! `start_pull`, which runs only on a user's POST.

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, HeaderName, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::{stream, StreamExt};
use genaimage_api::{
    CheckpointEnvironment, CheckpointLocation, CheckpointsResponse, InstalledLlmModel,
    LlmModelRole, LlmModelSource, LlmModelsResponse, LlmRoleModel, LlmRoleModelState, ModelPull,
    ModelPullListResponse, ModelPullState, StartModelPullRequest,
};
use genaimage_core::{
    models::{
        self, ModelListing, ModelSource, PullSnapshot, PullStartError, PullState, CHECKPOINT_LOADER,
    },
    HarnessError, LlmRole,
};
use tokio::time::timeout;

use crate::{AppState, Problem};

pub(crate) const PULL_BODY_LIMIT: usize = 4 * 1024;
const LISTING_DEADLINE: Duration = Duration::from_secs(10);
const CHECKPOINT_DEADLINE: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const COMPOSE_CHECKPOINTS: &str = "/opt/comfyui/models/checkpoints";

fn llm_unavailable(detail: impl Into<String>) -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        detail,
        "Check that the LLM service is running and that llm.base_url is correct, then try again.",
    )
    .with_type("urn:genaimage:problem:llm-unavailable")
}

fn llm_not_configured(state: &AppState) -> Problem {
    let detail = match &state.model_manager_error {
        Some(error) => format!("The configured LLM endpoint is not valid: {error}"),
        None => "No LLM endpoint is configured.".to_owned(),
    };
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        detail,
        "Set a valid llm.base_url in Settings and restart the server.",
    )
    .with_type("urn:genaimage:problem:llm-not-configured")
}

fn pull_not_found() -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        "No pull with this ID is known. Pulls are kept in server memory only, so they are forgotten when the server restarts.",
        "List the installed models to see whether the model arrived, or start the pull again.",
    )
    .with_type("urn:genaimage:problem:pull-not-found")
}

fn role_models(state: &AppState, listing: &ModelListing) -> Vec<LlmRoleModel> {
    [
        (LlmModelRole::Text, LlmRole::Text),
        (LlmModelRole::Vision, LlmRole::Vision),
    ]
    .map(|(api_role, role)| {
        let configured = state.llm_roles().model(role);
        let (model, status) = if configured.is_empty() {
            (None, LlmRoleModelState::NotConfigured)
        } else if listing.models.iter().any(|model| model.name == configured) {
            (Some(configured.to_owned()), LlmRoleModelState::Present)
        } else {
            (Some(configured.to_owned()), LlmRoleModelState::Missing)
        };
        LlmRoleModel {
            role: api_role,
            model,
            state: status,
        }
    })
    .into()
}

#[utoipa::path(
    get, path = "/models/llm",
    operation_id = "models_llm_list",
    tag = "models",
    description = "List installed LLM models and check the configured text and vision models against them (exact name match). `source` is `ollama` when the Ollama native API answered and `openai` for the `/v1/models` fallback, where pulls are unsupported. Never downloads anything.",
    responses(
        (status = 200, body = genaimage_api::LlmModelsResponse),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "LLM not configured or unreachable", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_llm(
    State(state): State<AppState>,
) -> Result<Json<LlmModelsResponse>, Problem> {
    let manager = state
        .model_manager
        .clone()
        .ok_or_else(|| llm_not_configured(&state))?;
    let listing = timeout(LISTING_DEADLINE, manager.list_installed())
        .await
        .map_err(|_| llm_unavailable("Listing the installed models timed out."))?
        .map_err(|error| llm_unavailable(error.to_string()))?;
    let roles = role_models(&state, &listing);
    Ok(Json(LlmModelsResponse {
        source: match listing.source {
            ModelSource::Ollama => LlmModelSource::Ollama,
            ModelSource::OpenAi => LlmModelSource::Openai,
        },
        pull_supported: listing.pull_supported,
        pull_unsupported_reason: listing.pull_unsupported_reason,
        models: listing
            .models
            .into_iter()
            .map(|model| InstalledLlmModel {
                name: model.name,
                size_bytes: model.size_bytes,
                digest: model.digest,
                modified_at: model.modified_at,
            })
            .collect(),
        roles,
    }))
}

fn api_pull(snapshot: PullSnapshot) -> ModelPull {
    ModelPull {
        id: snapshot.id,
        name: snapshot.name,
        state: match snapshot.state {
            PullState::Running => ModelPullState::Running,
            PullState::Succeeded => ModelPullState::Succeeded,
            PullState::Failed => ModelPullState::Failed,
            PullState::Cancelled => ModelPullState::Cancelled,
        },
        status: snapshot.status,
        layer: snapshot.layer,
        total_bytes: snapshot.total_bytes,
        completed_bytes: snapshot.completed_bytes,
        error: snapshot.error,
        started_at: snapshot.started_at,
        finished_at: snapshot.finished_at,
        sequence: snapshot.sequence,
    }
}

fn location(id: &str) -> [(HeaderName, String); 1] {
    [(header::LOCATION, format!("/api/v1/models/llm/pulls/{id}"))]
}

#[utoipa::path(
    post, path = "/models/llm/pulls",
    operation_id = "models_llm_pull_start",
    tag = "models",
    description = "Start downloading one LLM model. This is the only operation that downloads, and it runs only on this explicit request: never at startup, on saving settings, in doctor or after a missing-model error. Returns 202 once the endpoint accepted the request; observe it through the pull resources. Errors: 409 when this model is already being pulled (the problem names the pull), 422 for an invalid name, 429 when too many pulls run, 501 when the endpoint has no native pull API.",
    request_body = genaimage_api::StartModelPullRequest,
    responses(
        (status = 202, body = genaimage_api::ModelPull, headers(("Location" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "This model is already being pulled", body = genaimage_api::ProblemDetails, content_type = "application/problem+json", headers(("Location" = String))),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Invalid model name", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 429, description = "Too many pulls are running", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 501, description = "The endpoint has no native pull API", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "LLM not configured or unreachable", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn start_pull(
    State(state): State<AppState>,
    request: Result<Json<StartModelPullRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(error) => {
            return Problem::new(
                error.status(),
                "The request is not a JSON object with a model name.",
                "Send application/json such as {\"name\": \"llama3.2:3b\"}.",
            )
            .into_response()
        }
    };
    let Some(manager) = state.model_manager.clone() else {
        return llm_not_configured(&state).into_response();
    };
    let roles = state.llm_roles().clone();
    match state
        .pulls
        .start(manager, &request.name, move || roles.invalidate_models())
        .await
    {
        Ok(snapshot) => {
            let headers = location(&snapshot.id);
            (StatusCode::ACCEPTED, headers, Json(api_pull(snapshot))).into_response()
        }
        Err(PullStartError::InvalidName(error)) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            "Use the form [namespace/]name[:tag], for example llama3.2:3b.",
        )
        .into_response(),
        Err(PullStartError::InProgress(existing)) => (
            location(&existing.id),
            Problem::new(
                StatusCode::CONFLICT,
                format!(
                    "'{}' is already being pulled (pull {}).",
                    existing.name, existing.id
                ),
                "Follow that pull instead of starting another.",
            )
            .with_type("urn:genaimage:problem:pull-in-progress"),
        )
            .into_response(),
        Err(error @ PullStartError::TooManyPulls { .. }) => Problem::new(
            StatusCode::TOO_MANY_REQUESTS,
            error.to_string(),
            "Wait for a running pull to finish or cancel it.",
        )
        .with_type("urn:genaimage:problem:too-many-pulls")
        .into_response(),
        Err(PullStartError::ShuttingDown) => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is shutting down.",
            "Start the pull again after the server restarts.",
        )
        .into_response(),
        Err(PullStartError::Backend(HarnessError::UnsupportedOperation(reason))) => Problem::new(
            StatusCode::NOT_IMPLEMENTED,
            reason,
            "Install the model with your LLM server's own tooling, then list the models again.",
        )
        .with_type("urn:genaimage:problem:pull-unsupported")
        .into_response(),
        Err(PullStartError::Backend(error)) => llm_unavailable(error.to_string()).into_response(),
    }
}

#[utoipa::path(
    get, path = "/models/llm/pulls",
    operation_id = "models_llm_pull_list",
    tag = "models",
    description = "Pulls started since the server started, newest first. Finished pulls are kept for a bounded number only. Not persisted: after a restart the list is empty, which never means a model was installed.",
    responses(
        (status = 200, body = genaimage_api::ModelPullListResponse),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_pulls(State(state): State<AppState>) -> Json<ModelPullListResponse> {
    Json(ModelPullListResponse {
        pulls: state.pulls.list().into_iter().map(api_pull).collect(),
    })
}

#[utoipa::path(
    get, path = "/models/llm/pulls/{id}",
    operation_id = "models_llm_pull_get",
    tag = "models",
    description = "The current snapshot of a pull, with exact byte counts. 404 when the ID is unknown, including after a server restart.",
    params(("id" = String, Path, description = "Pull ID")),
    responses(
        (status = 200, body = genaimage_api::ModelPull),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Unknown pull", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get_pull(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ModelPull>, Problem> {
    state
        .pulls
        .get(&id)
        .map(|snapshot| Json(api_pull(snapshot)))
        .ok_or_else(pull_not_found)
}

#[utoipa::path(
    delete, path = "/models/llm/pulls/{id}",
    operation_id = "models_llm_pull_cancel",
    tag = "models",
    description = "Cancel a running pull. The response has state `cancelled`; a pull that already finished is returned unchanged. Cancelling closes the request to Ollama; partial downloads may remain and a later pull resumes them. A cancelled pull may still have installed the model if it finished just before the request closed, so `GET /models/llm` is the authoritative answer.",
    params(("id" = String, Path, description = "Pull ID")),
    responses(
        (status = 200, body = genaimage_api::ModelPull),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Unknown pull", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn cancel_pull(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ModelPull>, Problem> {
    let snapshot = state.pulls.cancel(&id).ok_or_else(pull_not_found)?;
    if snapshot.state == PullState::Cancelled {
        // The model may have finished installing before the request closed.
        state.llm_roles().invalidate_models();
    }
    Ok(Json(api_pull(snapshot)))
}

fn pull_event(pull: &ModelPull) -> Event {
    let name = match pull.state {
        ModelPullState::Running => "pull_progress",
        ModelPullState::Succeeded => "pull_succeeded",
        ModelPullState::Failed => "pull_failed",
        ModelPullState::Cancelled => "pull_cancelled",
    };
    Event::default()
        .id(pull.sequence.to_string())
        .event(name)
        .json_data(pull)
        .expect("a pull snapshot serialises")
}

#[utoipa::path(
    get, path = "/models/llm/pulls/{id}/events",
    operation_id = "models_llm_pull_events",
    tag = "models",
    description = "SSE stream for one pull; attach at any time, for example after a page reload. The first frame is the current snapshot. Event names: pull_progress (state running), then exactly one of pull_succeeded, pull_failed or pull_cancelled, after which the stream ends. Data is the ModelPull schema. Progress frames are coalesced, so intermediate snapshots may be skipped; the sequence increases. A stream that ends without a terminal event was cut: read the pull again. 404 when the ID is unknown.",
    params(("id" = String, Path, description = "Pull ID")),
    responses(
        (status = 200, description = "SSE frames; JSON data is ModelPull", body = String, content_type = "text/event-stream"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Unknown pull", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn pull_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let receiver = state.pulls.subscribe(&id).ok_or_else(pull_not_found)?;
    // The first frame is the current snapshot; later frames wait for a change.
    let frames = stream::unfold(Some((receiver, true)), |state| async move {
        let (mut receiver, first) = state?;
        if !first {
            tokio::time::sleep(PROGRESS_INTERVAL).await;
            // The sender lives as long as the pull entry; a closed channel ends the stream.
            receiver.changed().await.ok()?;
        }
        let pull = api_pull(receiver.borrow_and_update().clone());
        let event = pull_event(&pull);
        let next = (pull.state == ModelPullState::Running).then_some((receiver, false));
        Some((Ok::<_, Infallible>(event), next))
    })
    .take_until(async move { state.wait_for_shutdown().await });
    Ok((
        [
            (header::CACHE_CONTROL.as_str(), "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Sse::new(frames).keep_alive(
            KeepAlive::new()
                .interval(HEARTBEAT_INTERVAL)
                .text("heartbeat"),
        ),
    )
        .into_response())
}

fn locations() -> Vec<CheckpointLocation> {
    vec![
        CheckpointLocation {
            environment: CheckpointEnvironment::Compose,
            directory: COMPOSE_CHECKPOINTS.into(),
            steps: vec![
                "The directory is in the genaimage_comfyui-models volume of the ComfyUI service (see docs/compose.md).".into(),
                format!(
                    "Copy a file in with: docker compose cp <file> comfyui:{COMPOSE_CHECKPOINTS}/"
                ),
            ],
        },
        CheckpointLocation {
            environment: CheckpointEnvironment::Native,
            directory: "<ComfyUI>/models/checkpoints".into(),
            steps: vec!["Copy the file into models/checkpoints inside your ComfyUI folder.".into()],
        },
    ]
}

#[utoipa::path(
    get, path = "/models/checkpoints",
    operation_id = "models_checkpoints",
    tag = "models",
    description = "Checkpoints ComfyUI reports through the `ckpt_name` options of CheckpointLoaderSimple, plus where to put new files. GenAImage never downloads checkpoints. If ComfyUI cannot be reached, or does not list that node, the response is 503 and not an empty list.",
    responses(
        (status = 200, body = genaimage_api::CheckpointsResponse),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "ComfyUI unreachable or without the checkpoint loader", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn checkpoints(
    State(state): State<AppState>,
) -> Result<Json<CheckpointsResponse>, Problem> {
    let unavailable = |detail: String| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            detail,
            "Start ComfyUI, check comfyui.base_url, and run `genaimage doctor`. This is not an empty checkpoint list.",
        )
        .with_type("urn:genaimage:problem:comfyui-unavailable")
    };
    let checkpoints = timeout(
        CHECKPOINT_DEADLINE,
        models::list_checkpoints(&state.harness()),
    )
    .await
    .map_err(|_| unavailable(format!("ComfyUI did not list {CHECKPOINT_LOADER} in time.")))?
    .map_err(|error| unavailable(error.to_string()))?;
    Ok(Json(CheckpointsResponse {
        checkpoints,
        locations: locations(),
        refresh_note:
            "ComfyUI only sees new files after a refresh (press r in its web UI) or a restart."
                .into(),
    }))
}
