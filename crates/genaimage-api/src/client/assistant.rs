use std::pin::Pin;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{header, Method};

use super::{events::with_safe_preamble, has_content_type, Client, Error};
use crate::{
    AssistantAwaitingConfirmationEvent, AssistantConfirmationPreviewRequest,
    AssistantConfirmationPreviewResponse, AssistantConfirmationRequest,
    AssistantConfirmationRequiredEvent, AssistantConversationDetailQuery,
    AssistantConversationDetailResponse, AssistantConversationListQuery,
    AssistantConversationListResponse, AssistantConversationResponse, AssistantDeltaEvent,
    AssistantMessageStartedEvent, AssistantTerminalEvent, AssistantToolAuditQuery,
    AssistantToolAuditResponse, AssistantToolCallEvent, AssistantToolResultEvent,
    CreateAssistantConversationRequest, PromptAssistRequest, PromptAssistResponse,
    SendAssistantMessageRequest, SuggestWorkflowRequest, SuggestWorkflowResponse,
};

pub enum AssistantStreamEvent {
    MessageStarted(AssistantMessageStartedEvent),
    Delta(AssistantDeltaEvent),
    ToolCall(AssistantToolCallEvent),
    ToolResult(AssistantToolResultEvent),
    ConfirmationRequired(AssistantConfirmationRequiredEvent),
    AwaitingConfirmation(AssistantAwaitingConfirmationEvent),
    Done(AssistantTerminalEvent),
    Error(AssistantTerminalEvent),
}

pub type AssistantEventStream =
    Pin<Box<dyn Stream<Item = Result<AssistantStreamEvent, Error>> + Send>>;

async fn parse_assistant_stream(
    response: reqwest::Response,
) -> Result<AssistantEventStream, Error> {
    if !has_content_type(&response, "text/event-stream") {
        return Err(Error::UnexpectedEventContentType);
    }
    Ok(Box::pin(
        with_safe_preamble(response.bytes_stream())
            .eventsource()
            .map(|frame| {
                let frame = frame?;
                match frame.event.as_str() {
                    "message_started" => Ok(AssistantStreamEvent::MessageStarted(
                        serde_json::from_str(&frame.data)?,
                    )),
                    "delta" => Ok(AssistantStreamEvent::Delta(serde_json::from_str(
                        &frame.data,
                    )?)),
                    "tool_call" => Ok(AssistantStreamEvent::ToolCall(serde_json::from_str(
                        &frame.data,
                    )?)),
                    "tool_result" => Ok(AssistantStreamEvent::ToolResult(serde_json::from_str(
                        &frame.data,
                    )?)),
                    "confirmation_required" => Ok(AssistantStreamEvent::ConfirmationRequired(
                        serde_json::from_str(&frame.data)?,
                    )),
                    "awaiting_confirmation" => Ok(AssistantStreamEvent::AwaitingConfirmation(
                        serde_json::from_str(&frame.data)?,
                    )),
                    "done" => Ok(AssistantStreamEvent::Done(serde_json::from_str(
                        &frame.data,
                    )?)),
                    "error" => Ok(AssistantStreamEvent::Error(serde_json::from_str(
                        &frame.data,
                    )?)),
                    other => Err(Error::UnknownAssistantEvent(other.to_owned())),
                }
            }),
    ))
}

impl Client {
    pub async fn suggest_workflows(
        &self,
        body: &SuggestWorkflowRequest,
    ) -> Result<SuggestWorkflowResponse, Error> {
        self.json(
            self.request(Method::POST, &["assistant", "suggest"])?
                .json(body),
        )
        .await
    }

    /// Requests editable prompt proposals. The response never applies a value.
    pub async fn prompt_assist(
        &self,
        body: &PromptAssistRequest,
    ) -> Result<PromptAssistResponse, Error> {
        self.json(
            self.request(Method::POST, &["assistant", "prompt"])?
                .json(body),
        )
        .await
    }

    pub async fn create_assistant_conversation(
        &self,
        body: &CreateAssistantConversationRequest,
    ) -> Result<AssistantConversationResponse, Error> {
        self.json(
            self.request(Method::POST, &["assistant", "conversations"])?
                .json(body),
        )
        .await
    }

    pub async fn assistant_conversations(
        &self,
        query: &AssistantConversationListQuery,
    ) -> Result<AssistantConversationListResponse, Error> {
        self.json(self.get(&["assistant", "conversations"])?.query(query))
            .await
    }

    pub async fn assistant_conversation(
        &self,
        id: &str,
        query: &AssistantConversationDetailQuery,
    ) -> Result<AssistantConversationDetailResponse, Error> {
        self.json(self.get(&["assistant", "conversations", id])?.query(query))
            .await
    }

    pub async fn send_assistant_message(
        &self,
        id: &str,
        body: &SendAssistantMessageRequest,
    ) -> Result<AssistantEventStream, Error> {
        let request = self
            .request(
                Method::POST,
                &["assistant", "conversations", id, "messages"],
            )?
            .header(header::ACCEPT, "text/event-stream")
            .json(body);
        let response = self.send(request, false).await?;
        parse_assistant_stream(response).await
    }

    pub async fn preview_assistant_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        body: &AssistantConfirmationPreviewRequest,
    ) -> Result<AssistantConfirmationPreviewResponse, Error> {
        self.json(
            self.request(
                Method::POST,
                &[
                    "assistant",
                    "conversations",
                    conversation_id,
                    "confirmations",
                    confirmation_id,
                    "preview",
                ],
            )?
            .json(body),
        )
        .await
    }

    pub async fn decide_assistant_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        body: &AssistantConfirmationRequest,
    ) -> Result<AssistantEventStream, Error> {
        let request = self
            .request(
                Method::POST,
                &[
                    "assistant",
                    "conversations",
                    conversation_id,
                    "confirmations",
                    confirmation_id,
                ],
            )?
            .header(header::ACCEPT, "text/event-stream")
            .json(body);
        parse_assistant_stream(self.send(request, false).await?).await
    }

    pub async fn assistant_tool_calls(
        &self,
        conversation_id: &str,
        query: &AssistantToolAuditQuery,
    ) -> Result<AssistantToolAuditResponse, Error> {
        self.json(
            self.get(&["assistant", "conversations", conversation_id, "tool-calls"])?
                .query(query),
        )
        .await
    }

    pub async fn cancel_assistant_conversation(&self, id: &str) -> Result<(), Error> {
        self.send(
            self.request(Method::POST, &["assistant", "conversations", id, "cancel"])?,
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn delete_assistant_conversation(&self, id: &str) -> Result<(), Error> {
        self.send(
            self.request(Method::DELETE, &["assistant", "conversations", id])?,
            false,
        )
        .await?;
        Ok(())
    }
}
