use axum::{
    extract::{rejection::PathRejection, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use genaimage_api::{
    PackListResponse, PackModelCheck, PackNodeCheck, PackRequirementStatus, PackRequirements,
    PackSummary, PackValidationResponse, PackValidationStatus, WorkflowDiagnostic,
};
use genaimage_core::{
    pack::LoadedPack,
    pack_requirements::{PackStatus, RequirementStatus},
    LoadedWorkflow,
};

use crate::{AppState, Problem};

const MAX_DIAGNOSTICS: usize = 100;

fn internal_error(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
        "Check the pack files and server logs.",
    )
}

fn summarise(pack: &LoadedPack) -> PackSummary {
    let mut diagnostics: Vec<String> = pack
        .diagnostics
        .iter()
        .map(|item| item.message.clone())
        .collect();
    let mut workflow_ids = Vec::new();
    for path in &pack.workflow_paths {
        match LoadedWorkflow::load(path) {
            Ok(loaded) => workflow_ids.push(loaded.manifest.id),
            Err(error) => diagnostics.push(format!("{}: {error}", path.display())),
        }
    }
    let manifest = &pack.manifest;
    PackSummary {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        root: pack.root.display().to_string(),
        directory: pack.dir.display().to_string(),
        workflow_ids,
        templates: manifest.templates.clone(),
        requires: PackRequirements {
            node_classes: manifest.requires.node_classes.clone(),
            models: manifest.requires.models.clone(),
        },
        diagnostics,
    }
}

#[utoipa::path(
    get, path = "/packs",
    operation_id = "packs_list",
    tag = "packs",
    description = "List workflow packs with their declared workflows, templates and requirements, plus discovery diagnostics.",
    responses(
        (status = 200, body = genaimage_api::PackListResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(State(state): State<AppState>) -> Result<Json<PackListResponse>, Problem> {
    let harness = state.harness();
    let response = tokio::task::spawn_blocking(move || {
        let discovery = harness.discover_packs();
        let mut diagnostics: Vec<WorkflowDiagnostic> = discovery
            .diagnostics
            .iter()
            .map(|item| WorkflowDiagnostic {
                path: item.path.display().to_string(),
                error: item.message.clone(),
            })
            .collect();
        if diagnostics.len() > MAX_DIAGNOSTICS {
            let more = diagnostics.len() - MAX_DIAGNOSTICS;
            diagnostics.truncate(MAX_DIAGNOSTICS);
            diagnostics.push(WorkflowDiagnostic {
                path: String::new(),
                error: format!("{more} more diagnostics were omitted."),
            });
        }
        PackListResponse {
            packs: discovery.packs.iter().map(summarise).collect(),
            diagnostics,
        }
    })
    .await
    .map_err(internal_error)?;
    Ok(Json(response))
}

#[utoipa::path(
    get, path = "/packs/{id}/validate",
    operation_id = "packs_validate",
    tag = "packs",
    description = "Check a pack's declared node classes and models against ComfyUI /object_info. Reports missing and unknown requirements and an unreachable backend as such; never installs anything.",
    params(("id" = String, Path, description = "Pack ID")),
    responses(
        (status = 200, body = genaimage_api::PackValidationResponse),
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
pub(super) async fn validate(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
) -> Result<Json<PackValidationResponse>, Problem> {
    let AxumPath(id) = id.map_err(|error| {
        Problem::invalid_path(error, "Use a pack ID returned by the pack listing.")
    })?;
    let validation = state
        .harness()
        .validate_pack(&id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            Problem::new(
                StatusCode::NOT_FOUND,
                format!("Pack '{id}' was not found."),
                "Check the pack ID and the pack listing diagnostics; a duplicated or invalid pack is unavailable.",
            )
        })?;
    let status = |status: RequirementStatus| match status {
        RequirementStatus::Available => PackRequirementStatus::Available,
        RequirementStatus::Missing => PackRequirementStatus::Missing,
        RequirementStatus::Unknown => PackRequirementStatus::Unknown,
    };
    Ok(Json(PackValidationResponse {
        pack_id: validation.pack_id,
        status: match validation.status {
            PackStatus::Ok => PackValidationStatus::Ok,
            PackStatus::Missing => PackValidationStatus::Missing,
            PackStatus::Unknown => PackValidationStatus::Unknown,
            PackStatus::Unavailable => PackValidationStatus::Unavailable,
        },
        reason: validation.reason,
        node_classes: validation
            .node_classes
            .into_iter()
            .map(|node| PackNodeCheck {
                class: node.class,
                status: status(node.status),
            })
            .collect(),
        models: validation
            .models
            .into_iter()
            .map(|model| PackModelCheck {
                folder: model.folder,
                name: model.name,
                status: status(model.status),
                loader: model.loader,
                reason: model.reason,
            })
            .collect(),
    }))
}
