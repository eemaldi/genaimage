use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    Json,
};
use genaimage_core::assistant::suggest::{
    suggest_workflows, DroppedSuggestion, SuggestionError, SuggestionRequest, WorkflowSuggestion,
};

use crate::{AppState, Problem};

const DEFAULT_LIMIT: usize = 3;
const MAX_LIMIT: usize = 5;
const MAX_INTENT_CHARS: usize = 4_000;
const MAX_INTENT_BYTES: usize = 16 * 1024;

#[utoipa::path(
    post, path = "/assistant/suggest",
    operation_id = "assistant_suggest_workflows",
    tag = "assistant",
    description = "Rank and validate workflow and parameter proposals for an intent. Image parameters and presets that bind them are excluded from the LLM catalogue and cannot be selected by a suggestion. The returned values are a reviewable proposal only; this endpoint never submits or modifies a run.",
    request_body = genaimage_api::SuggestWorkflowRequest,
    responses(
        (status = 200, description = "Validated workflow suggestions; applied is always false", body = genaimage_api::SuggestWorkflowResponse),
        (status = 400, description = "Invalid suggestion request", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "The selected text role cannot accept the request", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "The LLM response was unavailable or malformed", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn suggest(
    State(state): State<AppState>,
    body: Result<Json<genaimage_api::SuggestWorkflowRequest>, JsonRejection>,
) -> Result<Json<genaimage_api::SuggestWorkflowResponse>, Problem> {
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with a non-empty intent and optional limit from 1 to 5.",
        )
    })?;
    if body.intent.trim().is_empty() {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "Intent must not be empty.",
            "Describe the generation intent in non-empty text.",
        ));
    }
    if body.intent.len() > MAX_INTENT_BYTES {
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Intent exceeds 16 KiB of UTF-8.",
            "Shorten the intent to at most 4,000 characters and 16 KiB.",
        ));
    }
    if body.intent.chars().count() > MAX_INTENT_CHARS {
        return Err(Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Intent exceeds 4,000 characters.",
            "Shorten the intent to at most 4,000 characters and 16 KiB.",
        ));
    }
    let limit = body.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "Suggestion limit must be from 1 to 5.",
            "Omit limit or send an integer from 1 to 5.",
        ));
    }
    let binding = state
        .llm_roles()
        .text()
        .await
        .map_err(Problem::assistant_unavailable)?;
    let harness = state.harness();
    let discovery = state
        .workflow_index
        .list(harness.clone())
        .await
        .map_err(|error| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
                "Check the workflow files and server logs.",
            )
        })?;
    let result = suggest_workflows(
        harness,
        discovery,
        binding,
        SuggestionRequest {
            intent: body.intent,
            limit,
        },
    )
    .await
    .map_err(suggestion_problem)?;
    Ok(Json(genaimage_api::SuggestWorkflowResponse {
        model: result.model,
        template: genaimage_api::AssistantPromptTemplate {
            name: result.template.name,
            sha256: result.template.sha256,
        },
        suggestions: result.suggestions.into_iter().map(suggestion).collect(),
        dropped: result.dropped.into_iter().map(dropped).collect(),
        truncated: result.truncated,
        applied: genaimage_api::AppliedFalse,
    }))
}

fn suggestion(value: WorkflowSuggestion) -> genaimage_api::WorkflowSuggestion {
    genaimage_api::WorkflowSuggestion {
        workflow_id: value.workflow_id,
        workflow_revision: value.workflow_revision,
        rank: value.rank,
        preset: value.preset,
        parameters: value.parameters,
        rationale: value.rationale,
        parameter_notes: value.parameter_notes,
    }
}

fn dropped(value: DroppedSuggestion) -> genaimage_api::DroppedWorkflowSuggestion {
    genaimage_api::DroppedWorkflowSuggestion {
        workflow_id: value.workflow_id,
        field: value.field,
        reason: value.reason,
    }
}

fn suggestion_problem(error: SuggestionError) -> Problem {
    match error {
        SuggestionError::InvalidRequest(error) => Problem::invalid_llm_request(error),
        SuggestionError::MalformedResponse(detail) => Problem::new(
            StatusCode::BAD_GATEWAY,
            format!("The LLM returned malformed suggestion JSON: {detail}"),
            "Review the configured text model or the editable assistant/suggest.md template, then retry.",
        ),
        SuggestionError::Provider(error) => Problem::new(
            StatusCode::BAD_GATEWAY,
            error.to_string(),
            "Check the local LLM service and retry the request without changing its reviewed content.",
        ),
        SuggestionError::Internal(error) => {
            tracing::error!(%error, "Workflow suggestion failed");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not prepare workflow suggestions.",
                "Check the workflow files, prompt template and server logs.",
            )
        }
    }
}
