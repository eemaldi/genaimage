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
    SettingError, WorkflowDetails, WorkflowDiagnostic, WorkflowImportConfirmRequest,
    WorkflowImportProposal, WorkflowImportRequest, WorkflowImportResponse, WorkflowInheritedInput,
    WorkflowInheritedSource, WorkflowListResponse, WorkflowNodeCheck, WorkflowNodeCheckStatus,
    WorkflowParameterError, WorkflowSummary, WorkflowValidationRequest, WorkflowValidationResponse,
};
use genaimage_core::{
    workflow_import::{ImportError, ImportIssue},
    Harness, HarnessError, LoadedWorkflow,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::{workflow_index::ThumbnailSource, AppState, Problem};

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

fn workflow_error(error: HarnessError) -> Problem {
    match error {
        HarnessError::WorkflowChanged => Problem::workflow_changed(),
        HarnessError::WorkflowAmbiguous(detail) => Problem::workflow_ambiguous(detail),
        error => internal_error(error),
    }
}

fn not_found(id: &str) -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        format!("Workflow '{id}' was not found."),
        "Check the workflow ID and inspect the workflow catalogue diagnostics.",
    )
}

async fn load(
    harness: std::sync::Arc<Harness>,
    id: String,
    revision: Option<String>,
) -> Result<LoadedWorkflow, Problem> {
    let search_id = id.clone();
    tokio::task::spawn_blocking(move || harness.load_api_workflow(&search_id, revision.as_deref()))
        .await
        .map_err(internal_error)?
        .map_err(workflow_error)?
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
    listing(state).await
}

#[utoipa::path(
    post, path = "/workflows/refresh",
    operation_id = "workflows_refresh",
    tag = "workflows",
    description = "Force a rebuild of the workflow catalogue, including every pack, and return the listing. The IDs `refresh` and `import` are reserved.",
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
pub(super) async fn refresh(
    State(state): State<AppState>,
) -> Result<Json<WorkflowListResponse>, Problem> {
    listing(state).await
}

async fn listing(state: AppState) -> Result<Json<WorkflowListResponse>, Problem> {
    let discovery = state
        .workflow_index
        .list(state.harness())
        .await
        .map_err(internal_error)?;
    Ok(Json(WorkflowListResponse {
        workflows: discovery
            .workflows
            .into_iter()
            .map(|workflow| {
                let source = workflow.source;
                let manifest = workflow.manifest;
                let thumbnail_url = manifest
                    .thumbnail
                    .as_ref()
                    .filter(|_| !genaimage_core::workflow::is_reserved_workflow_id(&manifest.id))
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
                    source: source.to_string(),
                    available: workflow.unavailable_reason.is_none(),
                    unavailable_reason: workflow.unavailable_reason,
                    revision: Some(workflow.revision),
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

fn issues(issues: Vec<ImportIssue>) -> Vec<SettingError> {
    issues
        .into_iter()
        .map(|issue| SettingError {
            key: issue.field,
            reason: issue.reason,
        })
        .collect()
}

fn import_error(error: ImportError) -> Problem {
    match error {
        ImportError::UiFormat => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This file is a ComfyUI UI-format export (it has 'nodes' and 'links'), which cannot be imported.",
            "Export the workflow in API format. In ComfyUI, use Workflow, then Export (API). In older versions, enable Dev mode in the settings and use Save (API Format).",
        )
        .with_type("urn:genaimage:problem:workflow-ui-format"),
        ImportError::InvalidGraph(found) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("The graph is not a valid ComfyUI API-format workflow ({} problem(s)).", found.len()),
            "Send an object keyed by node ID, where every node has a string class_type and an inputs object. Re-export the workflow in API format if unsure.",
        )
        .with_type("urn:genaimage:problem:workflow-import-invalid")
        .with_errors(issues(found)),
        error @ ImportError::GraphTooLarge { .. } => Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            error.to_string(),
            "Import a smaller workflow.",
        ),
        ImportError::GraphMismatch { expected } => Problem::new(
            StatusCode::CONFLICT,
            "graph_sha256 does not match the submitted graph.",
            "Send the graph unchanged together with the graph_sha256 of its proposal, or request a new proposal.",
        )
        .with_type("urn:genaimage:problem:workflow-graph-mismatch")
        .with_expected_graph_sha256(expected),
        ImportError::InvalidManifest(found) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("The manifest is invalid ({} problem(s)); nothing was written.", found.len()),
            "Fix the listed fields and confirm again. Nothing is corrected automatically.",
        )
        .with_type("urn:genaimage:problem:workflow-manifest-invalid")
        .with_errors(issues(found)),
        ImportError::Exists { id, suggested_id } => Problem::new(
            StatusCode::CONFLICT,
            format!("Workflow '{id}' already exists; nothing was written."),
            format!("Choose another id, for example '{suggested_id}'. Existing workflows are never overwritten."),
        )
        .with_type("urn:genaimage:problem:workflow-exists")
        .with_suggested_id(suggested_id),
        ImportError::Harness(error) => workflow_error(error),
    }
}

fn body_rejection(error: JsonRejection) -> Problem {
    Problem::new(
        error.status(),
        error.body_text(),
        "Send a JSON object as documented for this endpoint.",
    )
}

pub(crate) const IMPORT_BODY_LIMIT: usize = 2 * 1024 * 1024;
pub(crate) const IMPORT_CONFIRM_BODY_LIMIT: usize = 4 * 1024 * 1024;

#[utoipa::path(
    post, path = "/workflows/import",
    operation_id = "workflows_import",
    tag = "workflows",
    description = "Analyse a ComfyUI API-format graph and propose a version 2 manifest. Read-only: nothing is written. The ID `import` is reserved. Send `{graph, name?}`; the body is limited to 2 MiB. A UI-format export is refused with the problem type `urn:genaimage:problem:workflow-ui-format`.",
    request_body = genaimage_api::WorkflowImportRequest,
    responses(
        (status = 200, body = genaimage_api::WorkflowImportProposal),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "UI-format export (`workflow-ui-format`) or invalid graph (`workflow-import-invalid`)", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn import(
    State(state): State<AppState>,
    request: Result<Json<WorkflowImportRequest>, JsonRejection>,
) -> Result<Json<WorkflowImportProposal>, Problem> {
    let Json(request) = request.map_err(body_rejection)?;
    let harness = state.harness();
    let proposal = tokio::task::spawn_blocking(move || {
        harness.propose_workflow_import(&request.graph, request.name.as_deref())
    })
    .await
    .map_err(internal_error)?
    .map_err(import_error)?;
    let value = serde_json::to_value(proposal).map_err(internal_error)?;
    Ok(Json(serde_json::from_value(value).map_err(internal_error)?))
}

#[utoipa::path(
    post, path = "/workflows/import/confirm",
    operation_id = "workflows_import_confirm",
    tag = "workflows",
    description = "Write a confirmed import as a new user workflow: `<paths.workflows>/<id>/api-workflow.json` and `workflow.genaimage.json`, the manifest last. The manifest is validated strictly with the discovery rules and never corrected. An existing id, in the user root or any pack, is refused with 409 `urn:genaimage:problem:workflow-exists` and a `suggested_id` field. A graph that differs from `graph_sha256` is refused with 409 `urn:genaimage:problem:workflow-graph-mismatch` and an `expected_graph_sha256` field. When another manifest declares the written ID, the 409 `urn:genaimage:problem:workflow-ambiguous` carries `id` and `manifest_path`: the workflow was written and stays unavailable until the duplicate is removed or renamed. Repeating an identical confirmation returns 200 with the same result. The body is limited to 4 MiB.",
    request_body = genaimage_api::WorkflowImportConfirmRequest,
    responses(
        (status = 200, body = genaimage_api::WorkflowImportResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "`workflow-exists`, `workflow-graph-mismatch` or `workflow-ambiguous`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`workflow-manifest-invalid`, `workflow-import-invalid` or `workflow-ui-format`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn import_confirm(
    State(state): State<AppState>,
    request: Result<Json<WorkflowImportConfirmRequest>, JsonRejection>,
) -> Result<Json<WorkflowImportResponse>, Problem> {
    let Json(request) = request.map_err(body_rejection)?;
    let harness = state.harness();
    let blocking = harness.clone();
    let imported = tokio::task::spawn_blocking(move || {
        let manifest = serde_json::to_value(&request.manifest)
            .map_err(|error| HarnessError::Workflow(error.to_string()))?;
        blocking.confirm_workflow_import(&request.graph, &request.graph_sha256, &manifest)
    })
    .await
    .map_err(internal_error)?
    .map_err(import_error)?;
    // Rebuild the catalogue so the new workflow is listed at once, and refuse
    // to report success for a workflow that discovery cannot use.
    let discovery = state
        .workflow_index
        .list(harness)
        .await
        .map_err(internal_error)?;
    match discovery
        .workflows
        .iter()
        .find(|workflow| workflow.manifest.id == imported.id)
    {
        Some(workflow) => {
            if let Some(reason) = &workflow.unavailable_reason {
                return Err(Problem::workflow_written_but_ambiguous(
                    &imported.id,
                    &imported.manifest_path.display().to_string(),
                    reason,
                ));
            }
        }
        None => {
            return Err(internal_error(format!(
                "Workflow '{}' was written to '{}' but discovery did not list it.",
                imported.id,
                imported.manifest_path.display()
            )))
        }
    }
    Ok(Json(WorkflowImportResponse {
        id: imported.id,
        revision: imported.revision,
        manifest_path: imported.manifest_path.display().to_string(),
        source: "user".to_owned(),
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
        (status = 409, description = "Workflow ID declared by several manifests", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
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
    let workflow = load(state.harness(), id, None).await?;
    #[derive(serde::Serialize)]
    struct Details<'a> {
        #[serde(flatten)]
        manifest: &'a genaimage_core::workflow::WorkflowManifest,
        revision: String,
    }
    let value = serde_json::to_value(Details {
        manifest: &workflow.manifest,
        revision: workflow.revision(),
    })
    .map_err(internal_error)?;
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
        (status = 409, description = "Workflow revision mismatch or workflow ID declared by several manifests", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
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
    let Json(request) = request.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send a JSON object with optional parameters, preset and workflow_revision.",
        )
    })?;
    let workflow = load(harness.clone(), id, request.workflow_revision).await?;
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
    let inherited_inputs = workflow
        .inherited_inputs(request.preset.as_deref(), &request.parameters)
        .map_err(|error| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                error.to_string(),
                "Select a preset declared by this workflow.",
            )
        })?
        .map(|(name, definition, value)| {
            let connection = value.as_array().and_then(|parts| match parts.as_slice() {
                [node, output] => Some((node.as_str()?, output.as_u64()?)),
                _ => None,
            });
            WorkflowInheritedInput {
                parameter: name.to_owned(),
                node: definition.node.clone(),
                input: definition.input.clone(),
                source: match connection {
                    Some((node, output)) => WorkflowInheritedSource::Connection {
                        node: node.to_owned(),
                        output,
                    },
                    None => WorkflowInheritedSource::Literal {
                        json: value.to_string(),
                    },
                },
            }
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
        revision: Some(workflow.revision()),
        valid: parameter_errors.is_empty()
            && node_check.status == WorkflowNodeCheckStatus::Available,
        parameter_errors,
        inherited_inputs,
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
        (status = 409, description = "Workflow ID declared by several manifests", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
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
    let index = state
        .workflow_index
        .get(harness.clone())
        .await
        .map_err(internal_error)?;
    let root = harness.config().workflows.clone();
    let search_id = id.clone();
    let image = tokio::task::spawn_blocking(move || {
        let source = index.source(&search_id)?;
        Ok::<_, HarnessError>(source.map(|source| read_thumbnail(&source, &root)))
    })
    .await
    .map_err(internal_error)?
    .map_err(workflow_error)?
    .ok_or_else(|| not_found(&id))?;
    let (content_type, bytes) = image.ok_or_else(|| {
        Problem::new(
            StatusCode::NOT_FOUND,
            format!("Thumbnail for workflow '{id}' is unavailable."),
            "Check that the manifest declares a supported image inside its workflow directory.",
        )
    })?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes))
}

/// Reads a thumbnail confined to the pack directory for a pack workflow, and
/// to the user workflow root for any other.
fn read_thumbnail(workflow: &ThumbnailSource, user_root: &Path) -> Option<(&'static str, Vec<u8>)> {
    let relative = Path::new(workflow.thumbnail.as_deref()?);
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
    let root = workflow.boundary.as_deref().unwrap_or(user_root);
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
    use super::{read_thumbnail, ThumbnailSource};

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
        let source = ThumbnailSource {
            manifest_path: loaded.manifest_path.clone(),
            thumbnail: loaded.manifest.thumbnail.clone(),
            boundary: None,
        };
        let (mime, bytes) = read_thumbnail(&source, &root).unwrap();
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
        let source = ThumbnailSource {
            manifest_path: loaded.manifest_path.clone(),
            thumbnail: loaded.manifest.thumbnail.clone(),
            boundary: None,
        };
        assert!(read_thumbnail(&source, &root).is_none());
    }
}
