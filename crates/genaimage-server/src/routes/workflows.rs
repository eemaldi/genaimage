use std::{
    io::Read,
    path::{Component, Path},
    time::Duration,
};

use axum::{
    extract::{rejection::JsonRejection, Path as AxumPath, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use genaimage_api::{
    WorkflowDetails, WorkflowDiagnostic, WorkflowListResponse, WorkflowNodeCheck,
    WorkflowNodeCheckStatus, WorkflowParameterError, WorkflowSummary, WorkflowValidationRequest,
    WorkflowValidationResponse,
};
use genaimage_core::LoadedWorkflow;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::{AppState, Problem};

const NODE_CHECK_DEADLINE: Duration = Duration::from_secs(2);
const MAX_THUMBNAIL_BYTES: u64 = 10 * 1024 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/workflows", get(list))
        .route("/workflows/{id}", get(details))
        .route("/workflows/{id}/validate", post(validate))
        .route("/workflows/{id}/thumbnail", get(thumbnail))
}

fn internal_error(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
        "Check the workflow files and server logs.",
    )
}

fn not_found(id: &str) -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        format!("Workflow '{id}' was not found."),
        "Check the workflow ID and inspect the workflow catalogue diagnostics.",
    )
}

async fn load(state: &AppState, id: String) -> Result<LoadedWorkflow, Problem> {
    let harness = state.harness.clone();
    let search_id = id.clone();
    tokio::task::spawn_blocking(move || harness.load_workflow_by_id(&search_id))
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&id))
}

async fn list(State(state): State<AppState>) -> Result<Json<WorkflowListResponse>, Problem> {
    let harness = state.harness.clone();
    let discovery = tokio::task::spawn_blocking(move || harness.discover_workflows())
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;
    Ok(Json(WorkflowListResponse {
        workflows: discovery
            .manifests
            .into_iter()
            .map(|manifest| {
                let thumbnail_url = manifest.thumbnail.as_ref().map(|_| {
                    format!(
                        "/api/v1/workflows/{}/thumbnail",
                        utf8_percent_encode(&manifest.id, NON_ALPHANUMERIC)
                    )
                });
                WorkflowSummary {
                    id: manifest.id,
                    name: manifest.title,
                    description: manifest.description,
                    tags: manifest.tags,
                    category: manifest.category,
                    thumbnail_url,
                    version: manifest.version,
                }
            })
            .collect(),
        diagnostics: discovery
            .diagnostics
            .into_iter()
            .map(|item| WorkflowDiagnostic {
                path: item.path.display().to_string(),
                error: item.error.to_string(),
            })
            .collect(),
    }))
}

async fn details(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<WorkflowDetails>, Problem> {
    let workflow = load(&state, id).await?;
    let value = serde_json::to_value(workflow.manifest).map_err(internal_error)?;
    Ok(Json(serde_json::from_value(value).map_err(internal_error)?))
}

async fn validate(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<WorkflowValidationRequest>, JsonRejection>,
) -> Result<Json<WorkflowValidationResponse>, Problem> {
    let workflow = load(&state, id).await?;
    let Json(request) = request.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send a JSON object with parameters and an optional preset.",
        )
    })?;
    let errors = workflow
        .validate_bindings(request.preset.as_deref(), &request.parameters)
        .map_err(|error| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                error.to_string(),
                "Select a preset declared by this workflow.",
            )
        })?;
    let parameter_errors: Vec<WorkflowParameterError> = errors
        .into_iter()
        .map(|error| WorkflowParameterError {
            parameter: error.parameter,
            reason: error.reason,
        })
        .collect();
    let node_check = match tokio::time::timeout(
        NODE_CHECK_DEADLINE,
        state.harness.missing_workflow_nodes(&workflow.workflow),
    )
    .await
    {
        Ok(Ok(missing_classes)) => WorkflowNodeCheck {
            status: if missing_classes.is_empty() {
                WorkflowNodeCheckStatus::Available
            } else {
                WorkflowNodeCheckStatus::Missing
            },
            missing_classes,
            reason: None,
        },
        Ok(Err(error)) => WorkflowNodeCheck {
            status: WorkflowNodeCheckStatus::Unavailable,
            missing_classes: Vec::new(),
            reason: Some(error.to_string()),
        },
        Err(_) => WorkflowNodeCheck {
            status: WorkflowNodeCheckStatus::Unavailable,
            missing_classes: Vec::new(),
            reason: Some("Node check timed out.".into()),
        },
    };
    Ok(Json(WorkflowValidationResponse {
        valid: parameter_errors.is_empty()
            && node_check.status == WorkflowNodeCheckStatus::Available,
        parameter_errors,
        node_check,
    }))
}

async fn thumbnail(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, Problem> {
    let workflow = load(&state, id.clone()).await?;
    let image = tokio::task::spawn_blocking(move || read_thumbnail(&workflow))
        .await
        .map_err(internal_error)?;
    let (content_type, bytes) = image.ok_or_else(|| {
        Problem::new(
            StatusCode::NOT_FOUND,
            format!("Thumbnail for workflow '{id}' is unavailable."),
            "Check that the manifest declares a supported image inside its workflow directory.",
        )
    })?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes))
}

fn read_thumbnail(workflow: &LoadedWorkflow) -> Option<(&'static str, Vec<u8>)> {
    let relative = Path::new(workflow.manifest.thumbnail.as_deref()?);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let directory = workflow.manifest_path.parent()?.canonicalize().ok()?;
    let target = directory.join(relative).canonicalize().ok()?;
    if !target.starts_with(&directory) {
        return None;
    }
    let mime = mime_guess::from_path(&target).first_raw()?;
    let content_type = match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        _ => return None,
    };
    let file = std::fs::File::open(target).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_THUMBNAIL_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
        return None;
    }
    Some((content_type, bytes))
}
