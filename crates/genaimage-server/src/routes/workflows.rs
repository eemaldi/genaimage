use super::BinaryData;
use std::{
    io::Read,
    path::{Component, Path},
    time::Duration,
};

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        Path as AxumPath, State,
    },
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use genaimage_api::{
    WorkflowDetails, WorkflowDiagnostic, WorkflowListResponse, WorkflowNodeCheck,
    WorkflowNodeCheckStatus, WorkflowParameterError, WorkflowSummary, WorkflowValidationRequest,
    WorkflowValidationResponse,
};
use genaimage_core::{Harness, HarnessError, LoadedWorkflow};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::{AppState, Problem, WorkflowIndex};

const NODE_CHECK_DEADLINE: Duration = Duration::from_secs(2);
const MAX_THUMBNAIL_BYTES: u64 = 10 * 1024 * 1024;

fn parse_id(id: Result<AxumPath<String>, PathRejection>) -> Result<String, Problem> {
    id.map(|AxumPath(id)| id).map_err(|error| {
        Problem::invalid_path(
            error,
            "Use a workflow ID returned by the workflow catalogue.",
        )
    })
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

async fn load(harness: std::sync::Arc<Harness>, id: String) -> Result<LoadedWorkflow, Problem> {
    let search_id = id.clone();
    tokio::task::spawn_blocking(move || harness.load_workflow_by_id(&search_id))
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&id))
}

#[utoipa::path(
    get, path = "/workflows",
    operation_id = "workflows_list",
    tag = "workflows",
    description = "List workflows and discovery diagnostics.",
    responses(
        (status = 200, body = genaimage_api::WorkflowListResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(
    State(state): State<AppState>,
) -> Result<Json<WorkflowListResponse>, Problem> {
    let harness = state.harness();
    let index = state.workflow_index.clone();
    let discovery = tokio::task::spawn_blocking(move || {
        let mut index = index.lock().expect("workflow index lock poisoned");
        *index = None;
        let (new_index, discovery) = WorkflowIndex::rebuild(harness)?;
        *index = Some(new_index);
        Ok::<_, HarnessError>(discovery)
    })
    .await
    .map_err(internal_error)?
    .map_err(internal_error)?;
    Ok(Json(WorkflowListResponse {
        workflows: discovery
            .manifests
            .into_iter()
            .map(|manifest| {
                let thumbnail_url = manifest
                    .thumbnail
                    .as_ref()
                    .filter(|_| !matches!(manifest.id.as_str(), "." | ".."))
                    .map(|_| {
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

#[utoipa::path(
    get, path = "/workflows/{id}",
    operation_id = "workflows_details",
    tag = "workflows",
    description = "Inspect a workflow manifest.",
    params(("id" = String, Path, description = "Workflow ID")),
    responses(
        (status = 200, body = genaimage_api::WorkflowDetails),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn details(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
) -> Result<Json<WorkflowDetails>, Problem> {
    let id = parse_id(id)?;
    let workflow = load(state.harness(), id).await?;
    let value = serde_json::to_value(workflow.manifest).map_err(internal_error)?;
    let mut details: WorkflowDetails = serde_json::from_value(value).map_err(internal_error)?;
    for (name, parameter) in &mut details.parameters {
        parameter.declared_required =
            Some(workflow.manifest_source["parameters"][name]["required"].as_bool());
    }
    for (index, output) in details.outputs.iter_mut().enumerate() {
        output.declared_kind = Some(
            workflow.manifest_source["outputs"][index]["kind"]
                .as_str()
                .map(str::to_owned),
        );
    }
    Ok(Json(details))
}

#[utoipa::path(
    post, path = "/workflows/{id}/validate",
    operation_id = "workflows_validate",
    tag = "workflows",
    description = "Validate explicit workflow bindings without submitting a run.",
    params(("id" = String, Path, description = "Workflow ID")),
    request_body = genaimage_api::WorkflowValidationRequest,
    responses(
        (status = 200, body = genaimage_api::WorkflowValidationResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn validate(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
    request: Result<Json<WorkflowValidationRequest>, JsonRejection>,
) -> Result<Json<WorkflowValidationResponse>, Problem> {
    let id = parse_id(id)?;
    let harness = state.harness();
    let workflow = load(harness.clone(), id).await?;
    let Json(request) = request.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send a JSON object with parameters and an optional preset.",
        )
    })?;
    let errors = harness
        .validate_api_bindings(&workflow, request.preset.as_deref(), &request.parameters)
        .await
        .map_err(|error| {
            if matches!(error, HarnessError::UploadStorage(_)) {
                tracing::error!(%error, "Upload validation failed");
                return Problem::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The server could not validate the reference upload.",
                    "Check the server logs and the configured uploads directory.",
                );
            }
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
        harness.missing_workflow_nodes(&workflow.workflow),
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

#[utoipa::path(
    get, path = "/workflows/{id}/thumbnail",
    operation_id = "workflows_thumbnail",
    tag = "workflows",
    description = "Fetch a workflow thumbnail.",
    params(("id" = String, Path, description = "Workflow ID")),
    responses(
        (status = 200, description = "Workflow thumbnail image", content((BinaryData = "image/png"), (BinaryData = "image/jpeg"), (BinaryData = "image/gif"), (BinaryData = "image/webp"), (BinaryData = "image/avif"))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn thumbnail(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
) -> Result<impl IntoResponse, Problem> {
    let id = parse_id(id)?;
    let harness = state.harness();
    let index = state.workflow_index.clone();
    let search_id = id.clone();
    let lookup_harness = harness.clone();
    let workflow = tokio::task::spawn_blocking(move || {
        let mut index = index.lock().expect("workflow index lock poisoned");
        WorkflowIndex::load(&mut index, lookup_harness, &search_id)
    })
    .await
    .map_err(internal_error)?
    .map_err(internal_error)?
    .ok_or_else(|| not_found(&id))?;
    let root = harness.config().workflows.clone();
    let image = tokio::task::spawn_blocking(move || read_thumbnail(&workflow, &root))
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

fn read_thumbnail(workflow: &LoadedWorkflow, root: &Path) -> Option<(&'static str, Vec<u8>)> {
    let relative = Path::new(workflow.manifest.thumbnail.as_deref()?);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let mime = mime_guess::from_path(relative).first_raw()?;
    let content_type = match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        _ => return None,
    };
    let directory = workflow.manifest_path.parent()?;
    let file = genaimage_core::safe_file::open_regular_beneath(root, directory, relative)?;
    let mut bytes = Vec::new();
    file.take(MAX_THUMBNAIL_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
        return None;
    }
    Some((content_type, bytes))
}

#[cfg(all(test, unix))]
mod tests {
    use super::read_thumbnail;

    #[test]
    fn thumbnail_accepts_symlinked_root_with_parent_components() {
        let temp = tempfile::tempdir().unwrap();
        let actual = temp.path().join("actual");
        let nested = actual.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(temp.path().join("other")).unwrap();
        std::os::unix::fs::symlink(&actual, temp.path().join("alias")).unwrap();
        let root = temp.path().join("other/../alias");
        std::fs::write(
            nested.join("api.json"),
            br#"{ "1": {"class_type":"SaveImage", "inputs":{}} }"#,
        )
        .unwrap();
        std::fs::write(nested.join("workflow.genaimage.json"), br#"{ "schema_version":2, "id":"test", "version":"1", "workflow":"api.json", "thumbnail":"image.png" }"#).unwrap();
        std::fs::write(nested.join("image.png"), b"inside").unwrap();
        let loaded =
            genaimage_core::LoadedWorkflow::load(&root.join("nested/workflow.genaimage.json"))
                .unwrap();
        let (mime, bytes) = read_thumbnail(&loaded, &root).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"inside");
    }

    #[test]
    fn rejects_manifest_directory_swapped_after_loading() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workflows");
        let workflow = root.join("workflow");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(
            workflow.join("api.json"),
            br#"{ "1": {"class_type":"SaveImage", "inputs":{}} }"#,
        )
        .unwrap();
        std::fs::write(workflow.join("workflow.genaimage.json"), br#"{ "schema_version":2, "id":"test", "version":"1", "workflow":"api.json", "thumbnail":"image.png" }"#).unwrap();
        std::fs::write(outside.join("image.png"), b"outside").unwrap();
        let loaded =
            genaimage_core::LoadedWorkflow::load(&workflow.join("workflow.genaimage.json"))
                .unwrap();
        std::fs::rename(&workflow, root.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &workflow).unwrap();
        assert!(read_thumbnail(&loaded, &root).is_none());
    }
}
