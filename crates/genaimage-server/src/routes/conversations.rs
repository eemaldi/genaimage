use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::{stream, StreamExt};
use genaimage_api as api;
use genaimage_core::{
    assistant::{
        AssistantConfirmationDecision, AssistantConversation, AssistantConversationListQuery,
        AssistantMessage, AssistantMessageRole, AssistantMessageStatus, AssistantStream,
    },
    ChatContent, ChatDelta, ChatMessage, HarnessError, LlmRoleBinding, ToolCall,
};

use crate::{AppState, Problem};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_ASSISTANT_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_ASSISTANT_IMAGE_BASE64_BYTES: usize = 4 * MAX_ASSISTANT_IMAGE_BYTES.div_ceil(3);
pub(crate) const ASSISTANT_MESSAGE_BODY_LIMIT: usize = MAX_ASSISTANT_IMAGE_BASE64_BYTES + 64 * 1024;

#[utoipa::path(
    post, path = "/assistant/conversations",
    operation_id = "assistant_conversations_create",
    tag = "assistant",
    description = "Create a conversation pinned to the currently configured text role. The initial model is recorded explicitly; later assistant messages record the text model used for each turn.",
    request_body = api::CreateAssistantConversationRequest,
    responses(
        (status = 201, description = "Conversation created", body = api::AssistantConversationResponse, headers(("Location" = String, description = "Conversation URL"))),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn create(
    State(state): State<AppState>,
    body: Result<Json<api::CreateAssistantConversationRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let binding = text_binding(&state).await?;
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with a non-empty title.",
        )
    })?;
    let conversation = state
        .conversation_service()
        .create_conversation_with_model(body.title, binding.model().to_owned())
        .await
        .map_err(problem)?;
    let location = format!("/api/v1/assistant/conversations/{}", conversation.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(conversation_response(conversation)),
    ))
}

#[utoipa::path(
    get, path = "/assistant/conversations",
    operation_id = "assistant_conversations_list",
    tag = "assistant",
    description = "List conversations newest first. Results are bounded and cursor-paginated.",
    params(api::AssistantConversationListQuery),
    responses(
        (status = 200, description = "Conversation page", body = api::AssistantConversationListResponse),
        (status = 400, description = "Invalid query", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(
    State(state): State<AppState>,
    query: Result<Query<api::AssistantConversationListQuery>, QueryRejection>,
) -> Result<Json<api::AssistantConversationListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .conversation_service()
        .list_conversations(AssistantConversationListQuery {
            cursor: query.cursor,
            limit: query.limit,
        })
        .await
        .map_err(problem)?;
    Ok(Json(api::AssistantConversationListResponse {
        conversations: page
            .conversations
            .into_iter()
            .map(conversation_response)
            .collect(),
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    get, path = "/assistant/conversations/{id}",
    operation_id = "assistant_conversations_get",
    tag = "assistant",
    description = "Read a conversation and its newest bounded message history in sequence order.",
    params(("id" = String, Path, description = "Conversation ID"), api::AssistantConversationDetailQuery),
    responses(
        (status = 200, description = "Conversation detail", body = api::AssistantConversationDetailResponse),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Conversation not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::AssistantConversationDetailQuery>, QueryRejection>,
) -> Result<Json<api::AssistantConversationDetailResponse>, Problem> {
    let id = parse_id(id)?;
    let query = parse_query(query)?;
    let detail = state
        .conversation_service()
        .get_conversation(&id, query.message_limit)
        .await
        .map_err(problem)?;
    Ok(Json(detail_response(detail.conversation, detail.messages)))
}

#[utoipa::path(
    post, path = "/assistant/conversations/{id}/messages",
    operation_id = "assistant_conversations_send_message",
    tag = "assistant",
    description = "Append a user message and stream the assistant turn over SSE. A pending tool confirmation returns 409 until it is confirmed, edited or rejected. Image content is standard base64 PNG, JPEG or WebP with a 20 MiB decoded limit. Event names: message_started, delta, tool_call, tool_result, confirmation_required, awaiting_confirmation, done, error. Non-confirmation tools run after schema validation; confirmation tools pause without invocation. The terminal event contains the exact status and persisted assistant message ID when persistence succeeded; awaiting_confirmation is not success.",
    params(("id" = String, Path, description = "Conversation ID")),
    request_body = api::SendAssistantMessageRequest,
    responses(
        (status = 200, description = "SSE frames: message_started, delta, tool_call, then done or error", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Conversation not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Conversation already has a generation in flight", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "The selected text role cannot accept the request", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn send_message(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    body: Result<Json<api::SendAssistantMessageRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let binding = text_binding(&state).await?;
    let model = binding.model().to_owned();
    let id = parse_id(id)?;
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with a non-empty content array.",
        )
    })?;
    let assistant = state
        .conversation_service()
        .send_message_with_binding(
            &id,
            body.content
                .into_iter()
                .map(content_from_api)
                .collect::<Result<Vec<_>, _>>()?,
            binding,
        )
        .await
        .map_err(problem)?;
    Ok(sse_response(state, id, model, assistant))
}

#[utoipa::path(
    post, path = "/assistant/conversations/{id}/confirmations/{confirmation_id}",
    operation_id = "assistant_confirmations_decide", tag = "assistant",
    description = "Atomically confirm, edit or reject one pending tool call, then continue its turn over SSE. A decision is single-use.",
    params(("id" = String, Path, description = "Conversation ID"), ("confirmation_id" = String, Path, description = "Confirmation ID")),
    request_body = api::AssistantConfirmationRequest,
    responses(
        (status = 200, description = "SSE stream", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Invalid decision", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Confirmation not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Already decided or generation in flight", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Edited arguments are invalid or model capability is unsupported", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn decide_confirmation(
    State(state): State<AppState>,
    ids: Result<Path<(String, String)>, PathRejection>,
    body: Result<Json<api::AssistantConfirmationRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let Path((id, confirmation_id)) = ids.map_err(|error| {
        Problem::invalid_path(
            error,
            "Use conversation and confirmation IDs returned by the assistant API.",
        )
    })?;
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send one confirmation decision as application/json.",
        )
    })?;
    let decision = match body.decision {
        api::AssistantConfirmationDecisionKind::Confirm if body.arguments.is_none() && body.reason.is_none() => AssistantConfirmationDecision::Confirm,
        api::AssistantConfirmationDecisionKind::Edit => AssistantConfirmationDecision::Edit(body.arguments.ok_or_else(|| Problem::new(StatusCode::BAD_REQUEST, "Edit decisions require arguments.", "Send an arguments JSON object."))?),
        api::AssistantConfirmationDecisionKind::Reject if body.arguments.is_none() => AssistantConfirmationDecision::Reject(body.reason),
        _ => return Err(Problem::new(StatusCode::BAD_REQUEST, "Fields do not match the confirmation decision.", "Confirm accepts no extra fields; edit requires arguments; reject optionally accepts reason.")),
    };
    let binding = text_binding(&state).await?;
    let model = binding.model().to_owned();
    let assistant = state
        .conversation_service()
        .decide_confirmation_with_binding(&id, &confirmation_id, decision, binding)
        .await
        .map_err(problem)?;
    Ok(sse_response(state, id, model, assistant))
}

#[utoipa::path(
    get, path = "/assistant/conversations/{id}/tool-calls",
    operation_id = "assistant_tool_calls_list", tag = "assistant",
    description = "Read the bounded, append-only audit trail for tool proposals and invocations.",
    params(("id" = String, Path, description = "Conversation ID"), api::AssistantToolAuditQuery),
    responses(
        (status = 200, description = "Tool audit records", body = api::AssistantToolAuditResponse),
        (status = 400, description = "Invalid query", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Conversation not found", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn tool_calls(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::AssistantToolAuditQuery>, QueryRejection>,
) -> Result<Json<api::AssistantToolAuditResponse>, Problem> {
    let id = parse_id(id)?;
    let query = parse_query(query)?;
    let records = state
        .conversation_service()
        .list_tool_audit(&id, query.limit)
        .await
        .map_err(problem)?;
    Ok(Json(api::AssistantToolAuditResponse {
        tool_calls: records.into_iter().map(tool_audit_response).collect(),
    }))
}

#[utoipa::path(
    post, path = "/assistant/conversations/{id}/cancel",
    operation_id = "assistant_conversations_cancel",
    tag = "assistant",
    description = "Cancel an in-flight assistant turn and persist its partial response as cancelled.",
    params(("id" = String, Path, description = "Conversation ID")),
    responses(
        (status = 204, description = "Generation cancelled and persisted"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Conversation not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "No generation in flight", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn cancel(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, Problem> {
    let id = parse_id(id)?;
    state
        .conversation_service()
        .cancel(&id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/assistant/conversations/{id}",
    operation_id = "assistant_conversations_delete",
    tag = "assistant",
    description = "Delete a conversation. If generation is in flight, it is first cancelled and its terminal record is persisted before deletion.",
    params(("id" = String, Path, description = "Conversation ID")),
    responses(
        (status = 204, description = "Conversation deleted"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Conversation not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Conversation conflict", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, Problem> {
    let id = parse_id(id)?;
    state
        .conversation_service()
        .delete_conversation(&id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn text_binding(state: &AppState) -> Result<LlmRoleBinding, Problem> {
    state
        .llm_roles()
        .text()
        .await
        .map_err(Problem::assistant_unavailable)
}

fn sse_response(
    state: AppState,
    conversation_id: String,
    model: String,
    assistant: AssistantStream,
) -> Response {
    let user_message_id = assistant.user_message_id().to_owned();
    let initial = api::AssistantMessageStartedEvent {
        conversation_id: conversation_id.clone(),
        user_message_id: user_message_id.clone(),
        model: model.clone(),
    };
    let shutdown_conversation_id = conversation_id.clone();
    let shutdown_user_message_id = user_message_id.clone();
    let shutdown_model = model.clone();
    let terminal_service = state.conversation_service();
    let events = stream::once(async move {
        Ok::<_, Infallible>(
            Event::default()
                .event("message_started")
                .json_data(initial)
                .expect("serialisable assistant SSE event"),
        )
    })
    .chain(stream::unfold(Some(assistant), move |next| {
        let conversation_id = conversation_id.clone();
        let user_message_id = user_message_id.clone();
        let model = model.clone();
        let terminal_service = terminal_service.clone();
        async move {
            let mut assistant = next?;
            match assistant.next().await {
                Some(Ok(ChatDelta::Text(text))) => {
                    let event = Event::default()
                        .event("delta")
                        .json_data(api::AssistantDeltaEvent {
                            conversation_id,
                            user_message_id,
                            text,
                        })
                        .expect("serialisable assistant SSE event");
                    Some((Ok::<_, Infallible>(event), Some(assistant)))
                }
                Some(Ok(ChatDelta::ToolCall(tool_call))) => {
                    let event = Event::default()
                        .event("tool_call")
                        .json_data(api::AssistantToolCallEvent {
                            conversation_id,
                            user_message_id,
                            tool_call: tool_call_response(tool_call),
                        })
                        .expect("serialisable assistant SSE event");
                    Some((Ok::<_, Infallible>(event), Some(assistant)))
                }
                Some(Ok(ChatDelta::ToolResult {
                    tool_call_id,
                    tool,
                    ok,
                    result,
                    error,
                })) => {
                    let event = Event::default()
                        .event("tool_result")
                        .json_data(api::AssistantToolResultEvent {
                            conversation_id,
                            user_message_id,
                            tool_call_id,
                            tool,
                            ok,
                            result,
                            error,
                        })
                        .expect("serialisable assistant SSE event");
                    Some((Ok::<_, Infallible>(event), Some(assistant)))
                }
                Some(Ok(ChatDelta::ConfirmationRequired {
                    confirmation_id,
                    conversation_id: event_conversation_id,
                    tool_call_id,
                    tool,
                    arguments,
                    json_schema,
                    preview,
                })) => {
                    let event = Event::default()
                        .event("confirmation_required")
                        .json_data(api::AssistantConfirmationRequiredEvent {
                            confirmation_id,
                            status: api::AssistantToolConfirmationStatus::Pending,
                            conversation_id: event_conversation_id,
                            tool_call_id,
                            tool,
                            arguments,
                            json_schema,
                            preview,
                        })
                        .expect("serialisable assistant SSE event");
                    Some((Ok::<_, Infallible>(event), Some(assistant)))
                }
                Some(Err(_)) => {
                    tracing::warn!("assistant stream failed");
                    let terminal = assistant.terminal_message().await.ok();
                    let event = terminal_or_fallback_error_event(
                        conversation_id,
                        user_message_id,
                        model,
                        terminal,
                    );
                    Some((Ok::<_, Infallible>(event), None))
                }
                None => {
                    let terminal = match assistant.terminal_message().await {
                        Ok(terminal) => terminal,
                        Err(_) => {
                            tracing::error!("assistant terminal record was not available");
                            let event = terminal_or_fallback_error_event(
                                conversation_id,
                                user_message_id,
                                model,
                                None,
                            );
                            return Some((Ok::<_, Infallible>(event), None));
                        }
                    };
                    if terminal.status == AssistantMessageStatus::AwaitingConfirmation {
                        let pending = terminal_service
                            .pending_confirmation(&conversation_id)
                            .await;
                        let event = match pending {
                            Ok(Some(confirmation)) => Event::default()
                                .event("awaiting_confirmation")
                                .json_data(api::AssistantAwaitingConfirmationEvent {
                                    confirmation_id: confirmation.id,
                                    conversation_id,
                                    user_message_id,
                                    message_id: terminal.id,
                                    model: terminal.model.unwrap_or(model),
                                    status: api::AssistantMessageStatus::AwaitingConfirmation,
                                })
                                .expect("serialisable assistant SSE event"),
                            _ => terminal_or_fallback_error_event(
                                conversation_id,
                                user_message_id,
                                model,
                                Some(terminal),
                            ),
                        };
                        return Some((Ok::<_, Infallible>(event), None));
                    }
                    let kind = if terminal.status == AssistantMessageStatus::Complete {
                        "done"
                    } else {
                        "error"
                    };
                    let status = status_response(terminal.status);
                    let error = (terminal.status != AssistantMessageStatus::Complete)
                        .then(|| safe_terminal_error(terminal.status).to_owned());
                    let event = Event::default()
                        .event(kind)
                        .json_data(api::AssistantTerminalEvent {
                            conversation_id,
                            user_message_id,
                            message_id: Some(terminal.id),
                            model: terminal.model.unwrap_or(model),
                            status,
                            error,
                        })
                        .expect("serialisable assistant SSE event");
                    Some((Ok::<_, Infallible>(event), None))
                }
            }
        }
    }));
    let stream = stream::unfold(
        (Box::pin(events), Some(state)),
        move |(mut events, state)| {
            let conversation_id = shutdown_conversation_id.clone();
            let user_message_id = shutdown_user_message_id.clone();
            let model = shutdown_model.clone();
            async move {
                let state = state?;
                tokio::select! {
                    biased;
                    item = events.next() => item.map(|event| (event, (events, Some(state)))),
                    () = state.wait_for_shutdown() => {
                        let event = Event::default()
                            .event("error")
                            .json_data(api::AssistantTerminalEvent {
                                conversation_id,
                                user_message_id,
                                message_id: None,
                                model,
                                status: api::AssistantMessageStatus::Failed,
                                error: Some("The server shut down before the assistant stream completed.".into()),
                            })
                            .expect("serialisable assistant SSE event");
                        Some((Ok::<_, Infallible>(event), (events, None)))
                    }
                }
            }
        },
    );
    (
        [
            (header::CACHE_CONTROL.as_str(), "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(HEARTBEAT_INTERVAL)
                .text("heartbeat"),
        ),
    )
        .into_response()
}

fn terminal_or_fallback_error_event(
    conversation_id: String,
    user_message_id: String,
    model: String,
    terminal: Option<AssistantMessage>,
) -> Event {
    let payload = match terminal {
        Some(terminal) => api::AssistantTerminalEvent {
            conversation_id,
            user_message_id,
            message_id: Some(terminal.id),
            model: terminal.model.unwrap_or(model),
            status: status_response(terminal.status),
            error: Some(safe_terminal_error(terminal.status).to_owned()),
        },
        None => api::AssistantTerminalEvent {
            conversation_id,
            user_message_id,
            message_id: None,
            model,
            status: api::AssistantMessageStatus::Failed,
            error: Some("The assistant response could not be persisted.".into()),
        },
    };
    Event::default()
        .event("error")
        .json_data(payload)
        .expect("serialisable assistant SSE event")
}

fn safe_terminal_error(status: AssistantMessageStatus) -> &'static str {
    match status {
        AssistantMessageStatus::Complete => "The assistant response is complete.",
        AssistantMessageStatus::Cancelled => "The assistant response was cancelled.",
        AssistantMessageStatus::Failed => "The assistant response failed.",
        AssistantMessageStatus::AwaitingConfirmation => {
            "The assistant response is awaiting user confirmation."
        }
    }
}

fn parse_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, Problem> {
    query.map(|Query(query)| query).map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Check the query parameters and try again.",
        )
    })
}

fn parse_id(id: Result<Path<String>, PathRejection>) -> Result<String, Problem> {
    id.map(|Path(id)| id).map_err(|error| {
        Problem::invalid_path(
            error,
            "Use a conversation ID returned by the assistant API.",
        )
    })
}

fn problem(error: HarnessError) -> Problem {
    let (status, recovery) = match &error {
        HarnessError::ConversationNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "Use a conversation ID returned by the assistant API.",
        ),
        HarnessError::ConversationConflict(_) | HarnessError::ToolConfirmationConflict(_) => (
            StatusCode::CONFLICT,
            "Wait for the current generation, cancel it, or reload the conversation before retrying.",
        ),
        HarnessError::ToolConfirmationNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "Use a confirmation ID returned by this conversation.",
        ),
        HarnessError::InvalidToolArguments(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Edit the arguments to match the tool JSON schema and submit again.",
        ),
        HarnessError::InvalidConversationRequest(_) => (
            StatusCode::BAD_REQUEST,
            "Check the request fields and pagination bounds, then try again.",
        ),
        HarnessError::AssistantNotConfigured(_) | HarnessError::ShuttingDown => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Configure the text role and restart the server if required.",
        ),
        HarnessError::Configuration(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Use content and capabilities supported by the explicitly configured text model.",
        ),
        _ => {
            tracing::error!(%error, "Assistant API request failed");
            return Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not complete the assistant operation.",
                "Check the server logs and local state before retrying.",
            );
        }
    };
    Problem::new(status, error.to_string(), recovery)
}

fn conversation_response(value: AssistantConversation) -> api::AssistantConversationResponse {
    api::AssistantConversationResponse {
        id: value.id,
        title: value.title,
        created_at: value.created_at,
        updated_at: value.updated_at,
        model: value.model,
        system_prompt_template: value.system_prompt_template,
        system_prompt_sha256: value.system_prompt_sha256,
        system_prompt: value.system_prompt,
    }
}

fn detail_response(
    conversation: AssistantConversation,
    messages: Vec<AssistantMessage>,
) -> api::AssistantConversationDetailResponse {
    api::AssistantConversationDetailResponse {
        id: conversation.id,
        title: conversation.title,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
        model: conversation.model,
        system_prompt_template: conversation.system_prompt_template,
        system_prompt_sha256: conversation.system_prompt_sha256,
        system_prompt: conversation.system_prompt,
        messages: messages.into_iter().map(message_response).collect(),
    }
}

fn message_response(value: AssistantMessage) -> api::AssistantMessageResponse {
    api::AssistantMessageResponse {
        id: value.id,
        conversation_id: value.conversation_id,
        seq: value.seq,
        role: role_response(value.role),
        content: message_from_core(value.content),
        model: value.model,
        status: status_response(value.status),
        error: value.error,
        tool_call_id: value.tool_call_id,
        created_at: value.created_at,
    }
}

fn role_response(value: AssistantMessageRole) -> api::AssistantMessageRole {
    match value {
        AssistantMessageRole::User => api::AssistantMessageRole::User,
        AssistantMessageRole::Assistant => api::AssistantMessageRole::Assistant,
        AssistantMessageRole::Tool => api::AssistantMessageRole::Tool,
    }
}

fn status_response(value: AssistantMessageStatus) -> api::AssistantMessageStatus {
    match value {
        AssistantMessageStatus::Complete => api::AssistantMessageStatus::Complete,
        AssistantMessageStatus::Cancelled => api::AssistantMessageStatus::Cancelled,
        AssistantMessageStatus::Failed => api::AssistantMessageStatus::Failed,
        AssistantMessageStatus::AwaitingConfirmation => {
            api::AssistantMessageStatus::AwaitingConfirmation
        }
    }
}

fn content_from_api(value: api::AssistantChatContent) -> Result<ChatContent, Problem> {
    match value {
        api::AssistantChatContent::Text(text) => Ok(ChatContent::Text(text)),
        api::AssistantChatContent::Image { media_type, data } => {
            use base64::{engine::general_purpose::STANDARD, Engine as _};

            if data.len() > MAX_ASSISTANT_IMAGE_BASE64_BYTES {
                return Err(image_too_large());
            }
            let bytes = STANDARD
                .decode(data)
                .map_err(|_| image_unsupported_type())?;
            if bytes.len() > MAX_ASSISTANT_IMAGE_BYTES {
                return Err(image_too_large());
            }
            let detected = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                Some("image/png")
            } else if bytes.starts_with(b"\xff\xd8\xff") {
                Some("image/jpeg")
            } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                Some("image/webp")
            } else {
                None
            };
            if detected != Some(media_type.as_str()) {
                return Err(image_unsupported_type());
            }
            Ok(ChatContent::Image { media_type, bytes })
        }
    }
}

fn image_too_large() -> Problem {
    Problem::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "The decoded assistant image exceeds 20 MiB.",
        "Use one PNG, JPEG or WebP image no larger than 20 MiB.",
    )
}

fn image_unsupported_type() -> Problem {
    Problem::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "The assistant image is not valid base64 PNG, JPEG or WebP data.",
        "Encode one PNG, JPEG or WebP image as standard base64 and set its exact media_type.",
    )
}

fn content_to_api(value: ChatContent) -> api::AssistantChatContent {
    match value {
        ChatContent::Text(text) => api::AssistantChatContent::Text(text),
        ChatContent::Image { media_type, bytes } => {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            api::AssistantChatContent::Image {
                media_type,
                data: STANDARD.encode(bytes),
            }
        }
    }
}

fn message_from_core(value: ChatMessage) -> api::AssistantChatMessage {
    match value {
        ChatMessage::System(text) => api::AssistantChatMessage::System(text),
        ChatMessage::User(content) => {
            api::AssistantChatMessage::User(content.into_iter().map(content_to_api).collect())
        }
        ChatMessage::Assistant { text, tool_calls } => api::AssistantChatMessage::Assistant {
            text,
            tool_calls: tool_calls.into_iter().map(tool_call_response).collect(),
        },
        ChatMessage::Tool { call_id, result } => {
            api::AssistantChatMessage::Tool { call_id, result }
        }
    }
}

fn tool_call_response(value: ToolCall) -> api::AssistantToolCall {
    api::AssistantToolCall {
        id: value.id,
        name: value.name,
        arguments: value.arguments,
    }
}

fn tool_audit_response(
    value: genaimage_core::assistant::AssistantToolAuditRecord,
) -> api::AssistantToolAuditRecord {
    api::AssistantToolAuditRecord {
        id: value.id,
        conversation_id: value.conversation_id,
        confirmation_id: value.confirmation_id,
        confirmation_status: value.confirmation_status,
        tool_call_id: value.tool_call_id,
        tool: value.tool,
        proposed_arguments: value.proposed_arguments,
        validation_outcome: value.validation_outcome,
        validation_error: value.validation_error,
        decision: value.decision,
        effective_arguments: value.effective_arguments,
        decided_by: value.decided_by,
        invocation_ok: value.invocation_ok,
        invocation_result: value.invocation_result,
        invocation_error: value.invocation_error,
        created_at: value.created_at,
        decided_at: value.decided_at,
        invoked_at: value.invoked_at,
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;

    use super::problem;

    #[test]
    fn unsupported_tools_confirmation_is_unprocessable_without_changing_state() {
        let response = problem(genaimage_core::HarnessError::Configuration(
            "model 'text-model' lacks the required tools capability".into(),
        ))
        .into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
