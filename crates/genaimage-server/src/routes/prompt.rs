use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    Json,
};
use genaimage_api as api;
use genaimage_core::{
    assistant::prompt_assist::{
        DiffOperation, PromptAssistMode, PromptAssistRequest, PromptAssistService,
    },
    HarnessError,
};

use crate::{AppState, Problem};

pub(crate) const PROMPT_ASSIST_BODY_LIMIT: usize = 512 * 1024;

#[utoipa::path(
    post, path = "/assistant/prompt",
    operation_id = "assistant_prompt",
    tag = "assistant",
    description = "Create prompt proposals using the configured text model. The selected workflow is loaded by its exact ID and its description, tags and targeted parameter hints are included. Results are never applied: the client must separately present and explicitly accept a proposal. For negative mode, `original` is the existing negative prompt and the diff baseline; for every other mode the request `prompt` is the diff baseline.",
    request_body = api::PromptAssistRequest,
    responses(
        (status = 200, description = "Unapplied prompt proposals", body = api::PromptAssistResponse),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Workflow not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Invalid prompt-assist request", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Malformed or failed LLM response", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Assistant unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn assist(
    State(state): State<AppState>,
    body: Result<Json<api::PromptAssistRequest>, JsonRejection>,
) -> Result<Json<api::PromptAssistResponse>, Problem> {
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send one application/json prompt-assist request with the documented fields.",
        )
    })?;
    let workflow = match body.workflow_id.as_deref() {
        Some(id) => {
            let harness = state.harness();
            let workflow_id = id.to_owned();
            tokio::task::spawn_blocking(move || harness.load_workflow_by_id(&workflow_id))
                .await
                .map_err(internal_error)?
                .map_err(internal_error)?
                .ok_or_else(|| not_found(id))?
                .into()
        }
        None => None,
    };
    let request = PromptAssistRequest {
        mode: mode(body.mode),
        prompt: body.prompt,
        instruction: body.instruction,
        n: body.n,
        original: body.original,
        workflow_id: body.workflow_id,
        parameter: body.parameter,
    };
    let service =
        PromptAssistService::new(state.harness().config().data_dir()).map_err(internal_error)?;
    service
        .validate(&request, workflow.as_ref())
        .map_err(problem)?;
    let binding = state
        .llm_roles()
        .text()
        .await
        .map_err(Problem::assistant_unavailable)?;
    let response = service
        .assist(&binding, request, workflow.as_ref())
        .await
        .map_err(problem)?;
    Ok(Json(api::PromptAssistResponse {
        proposals: response
            .proposals
            .into_iter()
            .map(|proposal| api::PromptProposal {
                text: proposal.text,
                diff: proposal
                    .diff
                    .into_iter()
                    .map(|part| api::PromptDiff {
                        op: operation(part.op),
                        text: part.text,
                    })
                    .collect(),
            })
            .collect(),
        proposal_count: response.proposal_count,
        model: response.model,
        mode: api_mode(response.mode),
        template: api::PromptTemplateReference {
            name: response.template.name,
            sha256: response.template.sha256,
        },
        applied: false,
    }))
}

fn internal_error(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
        "Check the workflow and prompt template files and server logs.",
    )
}

fn not_found(id: &str) -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        format!("Workflow '{id}' was not found."),
        "Choose an exact workflow ID returned by the workflow catalogue, or omit workflow_id.",
    )
}

fn problem(error: HarnessError) -> Problem {
    let (status, recovery) = match &error {
        HarnessError::InvalidPromptAssistRequest(_) | HarnessError::Configuration(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Correct the prompt, mode-specific fields and selected workflow parameter.",
        ),
        HarnessError::PromptAssistProvider(_) => (
            StatusCode::BAD_GATEWAY,
            "Check the local LLM response format and connection, then request proposals again.",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Check the server logs and prompt template files.",
        ),
    };
    Problem::new(status, error.to_string(), recovery).with_type(
        if status == StatusCode::BAD_GATEWAY {
            "urn:genaimage:problem:prompt-assist-provider"
        } else if status == StatusCode::UNPROCESSABLE_ENTITY {
            "urn:genaimage:problem:prompt-assist-validation"
        } else {
            "about:blank"
        },
    )
}

fn mode(value: api::PromptAssistMode) -> PromptAssistMode {
    match value {
        api::PromptAssistMode::Expand => PromptAssistMode::Expand,
        api::PromptAssistMode::Refine => PromptAssistMode::Refine,
        api::PromptAssistMode::Negative => PromptAssistMode::Negative,
        api::PromptAssistMode::Variations => PromptAssistMode::Variations,
    }
}

fn operation(value: DiffOperation) -> api::PromptDiffOperation {
    match value {
        DiffOperation::Equal => api::PromptDiffOperation::Equal,
        DiffOperation::Insert => api::PromptDiffOperation::Insert,
        DiffOperation::Delete => api::PromptDiffOperation::Delete,
    }
}

fn api_mode(value: PromptAssistMode) -> api::PromptAssistMode {
    match value {
        PromptAssistMode::Expand => api::PromptAssistMode::Expand,
        PromptAssistMode::Refine => api::PromptAssistMode::Refine,
        PromptAssistMode::Negative => api::PromptAssistMode::Negative,
        PromptAssistMode::Variations => api::PromptAssistMode::Variations,
    }
}
