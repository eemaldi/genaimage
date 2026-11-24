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
#[serde(deny_unknown_fields)]
pub struct SuggestWorkflowRequest {
    /// At most 4,000 Unicode characters and 16 KiB of UTF-8.
    #[schema(max_length = 4000)]
    pub intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 5)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct SuggestWorkflowResponse {
    pub model: String,
    pub template: AssistantPromptTemplate,
    #[schema(max_items = 5)]
    pub suggestions: Vec<WorkflowSuggestion>,
    pub dropped: Vec<DroppedWorkflowSuggestion>,
    pub truncated: bool,
    /// Always false. This endpoint proposes values and never submits a run.
    pub applied: AppliedFalse,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppliedFalse;

impl Serialize for AppliedFalse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for AppliedFalse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = bool::deserialize(deserializer)?;
        if value {
            Err(serde::de::Error::custom("applied must be false"))
        } else {
            Ok(Self)
        }
    }
}

impl utoipa::PartialSchema for AppliedFalse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::Boolean)
            .enum_values(Some([false]))
            .into()
    }
}

impl ToSchema for AppliedFalse {}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantPromptTemplate {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowSuggestion {
    pub workflow_id: String,
    /// Opaque revision of the workflow snapshot used for validation.
    pub workflow_revision: String,
    #[schema(minimum = 1, maximum = 5)]
    pub rank: usize,
    pub preset: Option<String>,
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub parameter_notes: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct DroppedWorkflowSuggestion {
    pub workflow_id: String,
    pub field: String,
    pub reason: String,
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
    AwaitingConfirmation,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantChatContent {
    Text(String),
    Image {
        media_type: String,
        /// Standard base64-encoded image bytes. PNG, JPEG and WebP are accepted, up to 20 MiB decoded.
        data: String,
    },
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantConfirmationDecisionKind {
    Confirm,
    Edit,
    Reject,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantToolConfirmationStatus {
    Pending,
    Invoking,
    Succeeded,
    Failed,
    Rejected,
    OutcomeUnknown,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantConfirmationRequest {
    pub decision: AssistantConfirmationDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantToolAuditRecord {
    pub id: i64,
    pub conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_status: Option<String>,
    pub tool_call_id: String,
    pub tool: String,
    pub proposed_arguments: serde_json::Value,
    pub validation_outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_error: Option<String>,
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_arguments: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_error: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssistantToolAuditQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantToolAuditResponse {
    pub tool_calls: Vec<AssistantToolAuditRecord>,
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
pub struct AssistantToolResultEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantConfirmationRequiredEvent {
    pub confirmation_id: String,
    pub status: AssistantToolConfirmationStatus,
    pub conversation_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub json_schema: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantAwaitingConfirmationEvent {
    pub confirmation_id: String,
    pub conversation_id: String,
    pub user_message_id: String,
    pub message_id: String,
    pub model: String,
    pub status: AssistantMessageStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct AssistantTerminalEvent {
    pub conversation_id: String,
    pub user_message_id: String,
    /// Omitted on an `error` event when no assistant message could be persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub model: String,
    pub status: AssistantMessageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AssistantChatContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    #[cfg(feature = "client")]
    pub fn image(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        use base64::Engine as _;

        Self::Image {
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;

    #[test]
    fn image_constructor_serializes_standard_base64_without_numeric_bytes() {
        let content = AssistantChatContent::image("image/png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            serde_json::to_value(content).unwrap(),
            serde_json::json!({
                "image": {
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo="
                }
            })
        );
    }
}

/// One prompt-assistance operation. Every result remains an unapplied proposal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PromptAssistMode {
    Expand,
    Refine,
    Negative,
    Variations,
}

/// Request prompt assistance without changing a workflow or generation value.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptAssistRequest {
    pub mode: PromptAssistMode,
    /// The positive prompt, at most 16,384 Unicode characters.
    #[schema(max_length = 16384)]
    pub prompt: String,
    /// Required and non-blank for `refine`; at most 8,192 Unicode characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_length = 8192)]
    pub instruction: Option<String>,
    /// Number of variations to request, from 1 through 8. Defaults to 4 and is invalid for other modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 8)]
    pub n: Option<usize>,
    /// Existing negative prompt used only by `negative` mode as the diff baseline; at most 16,384 Unicode characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_length = 16384)]
    pub original: Option<String>,
    /// Exact workflow ID whose description, tags and selected parameter hints are sent to the LLM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    /// Target parameter name within `workflow_id`, enabling its manifest hints in the LLM request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
}

/// Prompt proposal response. `applied` is always false.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct PromptAssistResponse {
    pub proposals: Vec<PromptProposal>,
    /// Exact number of retained proposals after empty and duplicate values are removed.
    pub proposal_count: usize,
    pub model: String,
    pub mode: PromptAssistMode,
    pub template: PromptTemplateReference,
    /// Always `false`; accepting and applying a proposal is a separate client action.
    #[schema(example = false)]
    pub applied: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct PromptProposal {
    pub text: String,
    pub diff: Vec<PromptDiff>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct PromptDiff {
    pub op: PromptDiffOperation,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptDiffOperation {
    Equal,
    Insert,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct PromptTemplateReference {
    pub name: String,
    pub sha256: String,
}
