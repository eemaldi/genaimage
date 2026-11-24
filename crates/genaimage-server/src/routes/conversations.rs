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
        AssistantConversation, AssistantConversationListQuery, AssistantMessage,
        AssistantMessageRole, AssistantMessageStatus, AssistantStream,
    },
    ChatContent, ChatDelta, ChatMessage, HarnessError, LlmRoleBinding, ToolCall,
};

use crate::{AppState, Problem};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

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
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(
    State(state): State<AppState>,
    query: Result<Query<api::AssistantConversationListQuery>, QueryRejection>,
) -> Result<Json<api::AssistantConversationListResponse>, Problem> {
    let _binding = text_binding(&state).await?;
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
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::AssistantConversationDetailQuery>, QueryRejection>,
) -> Result<Json<api::AssistantConversationDetailResponse>, Problem> {
    let _binding = text_binding(&state).await?;
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
    description = "Append a user message and stream the assistant turn over SSE. Event names: message_started, delta, tool_call, done, error. Tool calls are proposals and are never executed by this endpoint. The terminal event contains the persisted assistant message ID and status.",
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
            body.content.into_iter().map(content_from_api).collect(),
            binding,
        )
        .await
        .map_err(problem)?;
    Ok(sse_response(state, id, model, assistant))
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
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn cancel(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, Problem> {
    let _binding = text_binding(&state).await?;
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
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Text role unavailable", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, Problem> {
    let _binding = text_binding(&state).await?;
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
        model,
    };
    let stream = stream::once(async move {
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
        async move {
            let mut assistant = next?;
            loop {
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
                    return Some((Ok::<_, Infallible>(event), Some(assistant)));
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
                    return Some((Ok::<_, Infallible>(event), Some(assistant)));
                }
                Some(Err(_)) => continue,
                None => {
                    let terminal = match assistant.terminal_message().await {
                        Ok(terminal) => terminal,
                        Err(error) => {
                            tracing::error!(%error, "assistant terminal record was not available");
                            return None;
                        }
                    };
                    let kind = if terminal.status == AssistantMessageStatus::Complete {
                        "done"
                    } else {
                        "error"
                    };
                    let event = Event::default()
                        .event(kind)
                        .json_data(api::AssistantTerminalEvent {
                            conversation_id,
                            user_message_id,
                            message_id: terminal.id,
                            model: terminal.model.unwrap_or_default(),
                            status: status_response(terminal.status),
                            error: terminal.error,
                        })
                        .expect("serialisable assistant SSE event");
                    return Some((Ok::<_, Infallible>(event), None));
                }
                }
            }
        }
    }))
    .take_until(async move { state.wait_for_shutdown().await });
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
        HarnessError::ConversationConflict(_) => (
            StatusCode::CONFLICT,
            "Wait for the current generation, cancel it, or reload the conversation before retrying.",
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
    }
}

fn content_from_api(value: api::AssistantChatContent) -> ChatContent {
    match value {
        api::AssistantChatContent::Text(text) => ChatContent::Text(text),
        api::AssistantChatContent::Image { media_type, bytes } => {
            ChatContent::Image { media_type, bytes }
        }
    }
}

fn content_to_api(value: ChatContent) -> api::AssistantChatContent {
    match value {
        ChatContent::Text(text) => api::AssistantChatContent::Text(text),
        ChatContent::Image { media_type, bytes } => {
            api::AssistantChatContent::Image { media_type, bytes }
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
