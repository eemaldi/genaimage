use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAssistantConversationRequest {
    pub title: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssistantConversationListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssistantConversationDetailQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[param(minimum = 1, maximum = 500)]
    pub message_limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantConversationResponse {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Explicit model selected when this conversation was created.
    pub model: String,
    pub system_prompt_template: String,
    pub system_prompt_sha256: String,
    pub system_prompt: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantConversationListResponse {
    pub conversations: Vec<AssistantConversationResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantConversationDetailResponse {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    pub system_prompt_template: String,
    pub system_prompt_sha256: String,
    pub system_prompt: String,
    pub messages: Vec<AssistantMessageResponse>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantMessageResponse {
    pub id: String,
    pub conversation_id: String,
    pub seq: i64,
    pub role: AssistantMessageRole,
    pub content: AssistantChatMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AssistantMessageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageRole {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageStatus {
    Complete,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantChatContent {
    Text(String),
    Image { media_type: String, bytes: Vec<u8> },
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantChatMessage {
    System(String),
    User(Vec<AssistantChatContent>),
    Assistant {
        text: String,
        tool_calls: Vec<AssistantToolCall>,
    },
    Tool {
        call_id: String,
        result: serde_json::Value,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendAssistantMessageRequest {
    pub content: Vec<AssistantChatContent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantMessageStartedEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantDeltaEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantToolCallEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    pub tool_call: AssistantToolCall,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantTerminalEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    pub message_id: String,
    pub model: String,
    pub status: AssistantMessageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
