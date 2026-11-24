use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{persistence::RunRepository, ChatContent, ChatMessage, HarnessError, Result, ToolCall};

const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 100;
const DEFAULT_MESSAGE_LIMIT: usize = 200;
const MAX_MESSAGE_LIMIT: usize = 500;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageRole {
    User,
    Assistant,
    Tool,
}

impl AssistantMessageRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            _ => Err(HarnessError::Database(format!(
                "unknown assistant message role '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageStatus {
    Complete,
    Cancelled,
    Failed,
    AwaitingConfirmation,
}

impl AssistantMessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::AwaitingConfirmation => "awaiting_confirmation",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            "awaiting_confirmation" => Ok(Self::AwaitingConfirmation),
            _ => Err(HarnessError::Database(format!(
                "unknown assistant message status '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantConversation {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    pub system_prompt_template: String,
    pub system_prompt_sha256: String,
    pub system_prompt: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantMessage {
    pub id: String,
    pub conversation_id: String,
    pub seq: i64,
    pub role: AssistantMessageRole,
    pub content: ChatMessage,
    pub model: Option<String>,
    pub status: AssistantMessageStatus,
    pub error: Option<String>,
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantConversationDetail {
    #[serde(flatten)]
    pub conversation: AssistantConversation,
    pub messages: Vec<AssistantMessage>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

impl AssistantToolConfirmationStatus {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "invoking" => Ok(Self::Invoking),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "rejected" => Ok(Self::Rejected),
            "outcome_unknown" => Ok(Self::OutcomeUnknown),
            "expired" => Ok(Self::Expired),
            _ => Err(HarnessError::Database(format!(
                "unknown assistant tool confirmation status '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantToolConfirmation {
    pub id: String,
    pub conversation_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub proposed_arguments: serde_json::Value,
    pub json_schema: serde_json::Value,
    pub source_message_id: String,
    pub user_message_id: String,
    pub model_step: u32,
    pub status: AssistantToolConfirmationStatus,
    pub decision: Option<String>,
    pub effective_arguments: Option<serde_json::Value>,
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewAssistantConfirmation {
    pub conversation_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub proposed_arguments: serde_json::Value,
    pub json_schema: serde_json::Value,
    pub source_message_id: String,
    pub user_message_id: String,
    pub model_step: u32,
}

#[derive(Clone, Debug)]
pub struct NewAssistantToolAudit {
    pub conversation_id: String,
    pub confirmation_id: Option<String>,
    pub tool_call_id: String,
    pub tool: String,
    pub proposed_arguments: serde_json::Value,
    pub validation_outcome: String,
    pub validation_error: Option<String>,
    pub decision: String,
    pub effective_arguments: Option<serde_json::Value>,
    pub decided_by: Option<String>,
    pub invocation_ok: Option<bool>,
    pub invocation_result: Option<serde_json::Value>,
    pub invocation_error: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub invoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewAssistantToolSettlement {
    pub ok: Option<bool>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantToolAuditRecord {
    pub id: i64,
    pub conversation_id: String,
    pub confirmation_id: Option<String>,
    pub confirmation_status: Option<String>,
    pub tool_call_id: String,
    pub tool: String,
    pub proposed_arguments: serde_json::Value,
    pub validation_outcome: String,
    pub validation_error: Option<String>,
    pub decision: String,
    pub effective_arguments: Option<serde_json::Value>,
    pub decided_by: Option<String>,
    pub invocation_ok: Option<bool>,
    pub invocation_result: Option<serde_json::Value>,
    pub invocation_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub invoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewAssistantConversation {
    pub title: String,
    pub model: String,
    pub system_prompt_template: String,
    pub system_prompt_sha256: String,
    pub system_prompt: String,
}

#[derive(Clone, Debug)]
pub struct NewAssistantMessage {
    pub role: AssistantMessageRole,
    pub content: ChatMessage,
    pub model: Option<String>,
    pub status: AssistantMessageStatus,
    pub error: Option<String>,
    pub tool_call_id: Option<String>,
}

impl NewAssistantMessage {
    pub fn user(content: Vec<ChatContent>) -> Self {
        Self {
            role: AssistantMessageRole::User,
            content: ChatMessage::User(content),
            model: None,
            status: AssistantMessageStatus::Complete,
            error: None,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: String, result: serde_json::Value) -> Self {
        Self {
            role: AssistantMessageRole::Tool,
            content: ChatMessage::Tool {
                call_id: call_id.clone(),
                result,
            },
            model: None,
            status: AssistantMessageStatus::Complete,
            error: None,
            tool_call_id: Some(call_id),
        }
    }

    pub fn assistant(
        model: String,
        text: String,
        tool_calls: Vec<ToolCall>,
        status: AssistantMessageStatus,
        error: Option<String>,
    ) -> Self {
        Self {
            role: AssistantMessageRole::Assistant,
            content: ChatMessage::Assistant { text, tool_calls },
            model: Some(model),
            status,
            error,
            tool_call_id: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AssistantConversationListQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssistantConversationPage {
    pub conversations: Vec<AssistantConversation>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct ConversationCursor(String, String);

impl ConversationCursor {
    fn decode(value: &str) -> Result<Self> {
        let invalid =
            || HarnessError::InvalidConversationRequest("Invalid conversation cursor.".into());
        let raw = value
            .strip_prefix("v1.")
            .filter(|raw| raw.len() <= 1024)
            .ok_or_else(invalid)?;
        let cursor: Self = serde_json::from_slice(&hex::decode(raw).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
        DateTime::parse_from_rfc3339(&cursor.0).map_err(|_| invalid())?;
        if cursor.1.is_empty() {
            return Err(invalid());
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String> {
        Ok(format!("v1.{}", hex::encode(serde_json::to_vec(self)?)))
    }
}

#[derive(Clone)]
pub struct ConversationStore {
    repository: RunRepository,
}

impl ConversationStore {
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let repository = tokio::task::spawn_blocking(move || RunRepository::open(&path))
            .await
            .map_err(|error| {
                HarnessError::Database(format!("state database task failed: {error}"))
            })??;
        Ok(Self { repository })
    }

    pub(crate) fn from_repository(repository: RunRepository) -> Self {
        Self { repository }
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(RunRepository) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || operation(repository))
            .await
            .map_err(|error| {
                HarnessError::Database(format!("state database task failed: {error}"))
            })?
    }

    pub async fn create(
        &self,
        conversation: NewAssistantConversation,
    ) -> Result<AssistantConversation> {
        self.call(move |repository| create(&repository, conversation))
            .await
    }

    pub async fn list(
        &self,
        query: AssistantConversationListQuery,
    ) -> Result<AssistantConversationPage> {
        self.call(move |repository| list(&repository, query)).await
    }

    pub async fn get(
        &self,
        conversation_id: &str,
        message_limit: Option<usize>,
    ) -> Result<AssistantConversationDetail> {
        let id = conversation_id.to_owned();
        self.call(move |repository| get(&repository, &id, message_limit))
            .await
    }

    pub async fn append_message(
        &self,
        conversation_id: &str,
        message: NewAssistantMessage,
    ) -> Result<AssistantMessage> {
        let id = conversation_id.to_owned();
        self.call(move |repository| append_message(&repository, &id, message))
            .await
    }

    pub async fn pending_confirmation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<AssistantToolConfirmation>> {
        let id = conversation_id.to_owned();
        self.call(move |repository| pending_confirmation(&repository, &id))
            .await
    }

    pub async fn create_confirmation(
        &self,
        new: NewAssistantConfirmation,
    ) -> Result<AssistantToolConfirmation> {
        self.call(move |repository| create_confirmation(&repository, new))
            .await
    }

    pub async fn get_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
    ) -> Result<AssistantToolConfirmation> {
        let conversation_id = conversation_id.to_owned();
        let confirmation_id = confirmation_id.to_owned();
        self.call(move |repository| {
            get_confirmation(&repository, &conversation_id, &confirmation_id)
        })
        .await
    }

    pub async fn claim_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        decision: &str,
        effective_arguments: Option<serde_json::Value>,
        reason: Option<String>,
    ) -> Result<AssistantToolConfirmation> {
        let conversation_id = conversation_id.to_owned();
        let confirmation_id = confirmation_id.to_owned();
        let decision = decision.to_owned();
        self.call(move |repository| {
            claim_confirmation(
                &repository,
                &conversation_id,
                &confirmation_id,
                &decision,
                effective_arguments,
                reason,
            )
        })
        .await
    }

    pub(crate) async fn settle_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        settlement: NewAssistantToolSettlement,
    ) -> Result<AssistantMessage> {
        let conversation_id = conversation_id.to_owned();
        let confirmation_id = confirmation_id.to_owned();
        self.call(move |repository| {
            settle_confirmation(&repository, &conversation_id, &confirmation_id, settlement)
        })
        .await
    }

    pub async fn recover_invoking_confirmations(&self, conversation_id: &str) -> Result<usize> {
        let conversation_id = conversation_id.to_owned();
        self.call(move |repository| recover_invoking_confirmations(&repository, &conversation_id))
            .await
    }

    pub async fn append_tool_audit(&self, audit: NewAssistantToolAudit) -> Result<()> {
        self.call(move |repository| append_tool_audit(&repository, audit))
            .await
    }

    pub async fn list_tool_audit(
        &self,
        conversation_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<AssistantToolAuditRecord>> {
        let conversation_id = conversation_id.to_owned();
        self.call(move |repository| list_tool_audit(&repository, &conversation_id, limit))
            .await
    }

    pub async fn get_message(&self, message_id: &str) -> Result<AssistantMessage> {
        let message_id = message_id.to_owned();
        self.call(move |repository| {
            repository.guarded_read(|connection| load_message(connection, &message_id))
        })
        .await
    }

    pub async fn set_message_status(
        &self,
        conversation_id: &str,
        message_id: &str,
        status: AssistantMessageStatus,
        error: Option<String>,
    ) -> Result<AssistantMessage> {
        let conversation_id = conversation_id.to_owned();
        let message_id = message_id.to_owned();
        self.call(move |repository| {
            repository.guarded(|connection| {
                let changed = connection.execute(
                    "UPDATE assistant_messages SET status = ?3, error = ?4 WHERE id = ?1 AND conversation_id = ?2",
                    params![message_id, conversation_id, status.as_str(), error],
                )?;
                if changed != 1 {
                    return Err(HarnessError::Database("assistant terminal message changed unexpectedly".into()));
                }
                connection.execute(
                    "UPDATE assistant_conversations SET updated_at = ?2 WHERE id = ?1",
                    params![conversation_id, timestamp()],
                )?;
                load_message(connection, &message_id)
            })
        }).await
    }

    pub async fn delete(&self, conversation_id: &str) -> Result<()> {
        let id = conversation_id.to_owned();
        self.call(move |repository| {
            repository.guarded(|connection| {
                let deleted = connection
                    .execute("DELETE FROM assistant_conversations WHERE id = ?1", [&id])?;
                if deleted == 0 {
                    return Err(HarnessError::ConversationNotFound {
                        conversation_id: id,
                    });
                }
                Ok(())
            })
        })
        .await
    }
}

fn pending_confirmation(
    repository: &RunRepository,
    conversation_id: &str,
) -> Result<Option<AssistantToolConfirmation>> {
    repository.guarded_read(|connection| {
        load_conversation(connection, conversation_id)?;
        connection
            .query_row(
                "SELECT id, conversation_id, tool_call_id, tool_name, proposed_arguments_json,
                        json_schema, source_message_id, user_message_id, model_step, status, decision,
                        effective_arguments_json, decision_reason, created_at, decided_at
                 FROM assistant_tool_confirmations
                 WHERE conversation_id = ?1 AND status = 'pending'
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                [conversation_id],
                map_confirmation,
            )
            .optional()
            .map_err(Into::into)
    })
}

fn create_confirmation(
    repository: &RunRepository,
    new: NewAssistantConfirmation,
) -> Result<AssistantToolConfirmation> {
    let id = Uuid::new_v4().to_string();
    let now = timestamp();
    repository.guarded(|connection| {
        load_conversation(connection, &new.conversation_id)?;
        connection.execute(
            "INSERT INTO assistant_tool_confirmations (
                id, conversation_id, tool_call_id, tool_name, proposed_arguments_json,
                json_schema, source_message_id, user_message_id, model_step, status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
            params![
                id,
                new.conversation_id,
                new.tool_call_id,
                new.tool,
                serde_json::to_string(&new.proposed_arguments)?,
                serde_json::to_string(&new.json_schema)?,
                new.source_message_id,
                new.user_message_id,
                new.model_step,
                now,
            ],
        )?;
        let marked = connection.execute(
            "INSERT INTO assistant_message_terminal_states (message_id, status)
             SELECT id, 'awaiting_confirmation' FROM assistant_messages
             WHERE id = ?1 AND conversation_id = ?2 AND status = 'complete'",
            params![new.source_message_id, new.conversation_id],
        )?;
        if marked != 1 {
            return Err(HarnessError::ToolConfirmationConflict(
                "The assistant proposal changed before its confirmation was persisted.".into(),
            ));
        }
        insert_tool_audit(
            connection,
            NewAssistantToolAudit {
                conversation_id: new.conversation_id.clone(),
                confirmation_id: Some(id.clone()),
                tool_call_id: new.tool_call_id.clone(),
                tool: new.tool.clone(),
                proposed_arguments: new.proposed_arguments.clone(),
                validation_outcome: "valid".into(),
                validation_error: None,
                decision: "pending".into(),
                effective_arguments: None,
                decided_by: None,
                invocation_ok: None,
                invocation_result: None,
                invocation_error: None,
                decided_at: None,
                invoked_at: None,
            },
        )?;
        load_confirmation(connection, &new.conversation_id, &id)
    })
}

fn get_confirmation(
    repository: &RunRepository,
    conversation_id: &str,
    confirmation_id: &str,
) -> Result<AssistantToolConfirmation> {
    repository
        .guarded_read(|connection| load_confirmation(connection, conversation_id, confirmation_id))
}

fn claim_confirmation(
    repository: &RunRepository,
    conversation_id: &str,
    confirmation_id: &str,
    decision: &str,
    effective_arguments: Option<serde_json::Value>,
    reason: Option<String>,
) -> Result<AssistantToolConfirmation> {
    if !matches!(decision, "confirm" | "edit" | "reject") {
        return Err(HarnessError::InvalidConversationRequest(
            "A confirmation decision must be confirm, edit or reject.".into(),
        ));
    }
    repository.guarded(|connection| {
        let current = load_confirmation(connection, conversation_id, confirmation_id)?;
        let status = if decision == "reject" {
            "rejected"
        } else {
            "invoking"
        };
        let changed = connection.execute(
            "UPDATE assistant_tool_confirmations
             SET status = ?3, decision = ?4, effective_arguments_json = ?5,
                 decision_reason = ?6, decided_at = ?7
             WHERE id = ?1 AND conversation_id = ?2 AND status = 'pending'",
            params![
                confirmation_id,
                conversation_id,
                status,
                decision,
                effective_arguments
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                reason,
                timestamp(),
            ],
        )?;
        if changed != 1 {
            return Err(HarnessError::ToolConfirmationConflict(format!(
                "confirmation '{}' is already in state {:?}",
                current.id, current.status
            )));
        }
        insert_tool_audit(
            connection,
            NewAssistantToolAudit {
                conversation_id: current.conversation_id.clone(),
                confirmation_id: Some(current.id.clone()),
                tool_call_id: current.tool_call_id.clone(),
                tool: current.tool.clone(),
                proposed_arguments: current.proposed_arguments.clone(),
                validation_outcome: "valid".into(),
                validation_error: None,
                decision: if decision == "reject" {
                    "rejected".into()
                } else {
                    "invocation_started".into()
                },
                effective_arguments: effective_arguments.clone(),
                decided_by: Some("user".into()),
                invocation_ok: None,
                invocation_result: None,
                invocation_error: None,
                decided_at: Some(Utc::now()),
                invoked_at: (decision != "reject").then(Utc::now),
            },
        )?;
        if decision == "reject" {
            clear_awaiting_state(connection, &current.source_message_id)?;
            let rejection = format!(
                "The user rejected tool '{}'.{}",
                current.tool,
                reason
                    .as_deref()
                    .map(|value| format!(" Reason: {value}"))
                    .unwrap_or_default()
            );
            append_tool_message_in_connection(
                connection,
                &current.conversation_id,
                &current.tool_call_id,
                serde_json::json!({"ok": false, "error": rejection}),
            )?;
        }
        load_confirmation(connection, conversation_id, confirmation_id)
    })
}

fn settle_confirmation(
    repository: &RunRepository,
    conversation_id: &str,
    confirmation_id: &str,
    settlement: NewAssistantToolSettlement,
) -> Result<AssistantMessage> {
    repository.guarded(|connection| {
        let current = load_confirmation(connection, conversation_id, confirmation_id)?;
        settle_confirmation_in_connection(connection, current, settlement)
    })
}

fn settle_confirmation_in_connection(
    connection: &rusqlite::Connection,
    current: AssistantToolConfirmation,
    settlement: NewAssistantToolSettlement,
) -> Result<AssistantMessage> {
    if current.status != AssistantToolConfirmationStatus::Invoking {
        return Err(HarnessError::ToolConfirmationConflict(format!(
            "confirmation '{}' is not invoking (current state {:?})",
            current.id, current.status
        )));
    }
    let (status, decision) = match settlement.ok {
        Some(true) => ("succeeded", "invocation_succeeded"),
        Some(false) => ("failed", "invocation_failed"),
        None => ("outcome_unknown", "outcome_unknown"),
    };
    let changed = connection.execute(
        "UPDATE assistant_tool_confirmations SET status = ?2 WHERE id = ?1 AND status = 'invoking'",
        params![current.id, status],
    )?;
    if changed != 1 {
        return Err(HarnessError::ToolConfirmationConflict(format!(
            "confirmation '{}' changed before its outcome was recorded",
            current.id
        )));
    }
    insert_tool_audit(
        connection,
        NewAssistantToolAudit {
            conversation_id: current.conversation_id.clone(),
            confirmation_id: Some(current.id.clone()),
            tool_call_id: current.tool_call_id.clone(),
            tool: current.tool.clone(),
            proposed_arguments: current.proposed_arguments.clone(),
            validation_outcome: "valid".into(),
            validation_error: None,
            decision: decision.into(),
            effective_arguments: current.effective_arguments.clone(),
            decided_by: Some("runtime".into()),
            invocation_ok: settlement.ok,
            invocation_result: settlement.result.clone(),
            invocation_error: settlement.error.clone(),
            decided_at: current.decided_at,
            invoked_at: Some(Utc::now()),
        },
    )?;
    clear_awaiting_state(connection, &current.source_message_id)?;
    let result = match settlement.ok {
        Some(true) => serde_json::json!({"ok": true, "result": settlement.result}),
        _ => serde_json::json!({"ok": false, "error": settlement.error}),
    };
    append_tool_message_in_connection(
        connection,
        &current.conversation_id,
        &current.tool_call_id,
        result,
    )
}

fn recover_invoking_confirmations(
    repository: &RunRepository,
    conversation_id: &str,
) -> Result<usize> {
    repository.guarded(|connection| {
        load_conversation(connection, conversation_id)?;
        let ids: Vec<String> = {
            let mut statement = connection.prepare(
                "SELECT id FROM assistant_tool_confirmations
                 WHERE conversation_id = ?1 AND status = 'invoking'
                 ORDER BY created_at ASC LIMIT 100",
            )?;
            let rows = statement.query_map([conversation_id], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            rows
        };
        for id in &ids {
            let current = load_confirmation(connection, conversation_id, id)?;
            settle_confirmation_in_connection(connection, current, NewAssistantToolSettlement {
                ok: None,
                result: None,
                error: Some("Outcome unknown. The process stopped after invocation started; check the external operation before continuing and never blindly retry it.".into()),
            })?;
        }
        Ok(ids.len())
    })
}

fn clear_awaiting_state(connection: &rusqlite::Connection, message_id: &str) -> Result<()> {
    let removed = connection.execute(
        "DELETE FROM assistant_message_terminal_states WHERE message_id = ?1",
        params![message_id],
    )?;
    if removed != 1 {
        return Err(HarnessError::ToolConfirmationConflict(
            "The assistant proposal changed while its confirmation was settled.".into(),
        ));
    }
    Ok(())
}

fn append_tool_message_in_connection(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    tool_call_id: &str,
    result: serde_json::Value,
) -> Result<AssistantMessage> {
    let id = Uuid::new_v4().to_string();
    let now = timestamp();
    let content = serde_json::to_string(&ChatMessage::Tool {
        call_id: tool_call_id.to_owned(),
        result,
    })?;
    let seq: i64 = connection.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM assistant_messages WHERE conversation_id = ?1",
        [conversation_id],
        |row| row.get(0),
    )?;
    connection.execute(
        "INSERT INTO assistant_messages (
            id, conversation_id, seq, role, content_json, model, status, error, tool_call_id, created_at
         ) VALUES (?1, ?2, ?3, 'tool', ?4, NULL, 'complete', NULL, ?5, ?6)",
        params![id, conversation_id, seq, content, tool_call_id, now],
    )?;
    connection.execute(
        "UPDATE assistant_conversations SET updated_at = ?2 WHERE id = ?1",
        params![conversation_id, now],
    )?;
    load_message(connection, &id)
}

fn append_tool_audit(repository: &RunRepository, audit: NewAssistantToolAudit) -> Result<()> {
    repository.guarded(|connection| insert_tool_audit(connection, audit))
}

fn insert_tool_audit(
    connection: &rusqlite::Connection,
    audit: NewAssistantToolAudit,
) -> Result<()> {
    connection.execute(
        "INSERT INTO assistant_tool_call_audit (
            conversation_id, confirmation_id, tool_call_id, tool_name, proposed_arguments_json,
            validation_outcome, validation_error, decision, effective_arguments_json, decided_by,
            invocation_ok, invocation_result_json, invocation_error, created_at, decided_at, invoked_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            audit.conversation_id, audit.confirmation_id, audit.tool_call_id, audit.tool,
            serde_json::to_string(&audit.proposed_arguments)?, audit.validation_outcome,
            audit.validation_error, audit.decision,
            audit.effective_arguments.as_ref().map(serde_json::to_string).transpose()?,
            audit.decided_by, audit.invocation_ok,
            audit.invocation_result.as_ref().map(serde_json::to_string).transpose()?,
            audit.invocation_error, timestamp(),
            audit.decided_at.map(|time| time.to_rfc3339_opts(SecondsFormat::Nanos, true)),
            audit.invoked_at.map(|time| time.to_rfc3339_opts(SecondsFormat::Nanos, true)),
        ],
    )?;
    Ok(())
}

fn list_tool_audit(
    repository: &RunRepository,
    conversation_id: &str,
    limit: Option<usize>,
) -> Result<Vec<AssistantToolAuditRecord>> {
    let limit = bounded_limit(limit, 100, 200, "Limit")?;
    repository.guarded_read(|connection| {
        load_conversation(connection, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, confirmation_id, tool_call_id, tool_name,
                    proposed_arguments_json, validation_outcome, validation_error, decision,
                    (SELECT status FROM assistant_tool_confirmations c
                     WHERE c.id = assistant_tool_call_audit.confirmation_id) AS confirmation_status,
                    effective_arguments_json, decided_by, invocation_ok, invocation_result_json,
                    invocation_error, created_at, decided_at, invoked_at
             FROM assistant_tool_call_audit WHERE conversation_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let records = statement
            .query_map(
                params![
                    conversation_id,
                    i64::try_from(limit).expect("bounded limit")
                ],
                map_tool_audit,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    })
}

fn load_confirmation(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    confirmation_id: &str,
) -> Result<AssistantToolConfirmation> {
    connection
        .query_row(
            "SELECT id, conversation_id, tool_call_id, tool_name, proposed_arguments_json,
                json_schema, source_message_id, user_message_id, model_step, status, decision,
                effective_arguments_json, decision_reason, created_at, decided_at
         FROM assistant_tool_confirmations WHERE id = ?1 AND conversation_id = ?2",
            params![confirmation_id, conversation_id],
            map_confirmation,
        )
        .optional()?
        .ok_or_else(|| HarnessError::ToolConfirmationNotFound {
            conversation_id: conversation_id.to_owned(),
            confirmation_id: confirmation_id.to_owned(),
        })
}

fn map_confirmation(row: &Row<'_>) -> rusqlite::Result<AssistantToolConfirmation> {
    let status = AssistantToolConfirmationStatus::parse(&row.get::<_, String>(9)?)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    Ok(AssistantToolConfirmation {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        tool_call_id: row.get(2)?,
        tool: row.get(3)?,
        proposed_arguments: parse_json(row, 4)?,
        json_schema: parse_json(row, 5)?,
        source_message_id: row.get(6)?,
        user_message_id: row.get(7)?,
        model_step: row.get(8)?,
        status,
        decision: row.get(10)?,
        effective_arguments: parse_json_optional(row, 11)?,
        decision_reason: row.get(12)?,
        created_at: parse_time(row, 13)?,
        decided_at: parse_time_optional(row, 14)?,
    })
}

fn map_tool_audit(row: &Row<'_>) -> rusqlite::Result<AssistantToolAuditRecord> {
    Ok(AssistantToolAuditRecord {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        confirmation_id: row.get(2)?,
        confirmation_status: row.get(9)?,
        tool_call_id: row.get(3)?,
        tool: row.get(4)?,
        proposed_arguments: parse_json(row, 5)?,
        validation_outcome: row.get(6)?,
        validation_error: row.get(7)?,
        decision: row.get(8)?,
        effective_arguments: parse_json_optional(row, 10)?,
        decided_by: row.get(11)?,
        invocation_ok: row.get(12)?,
        invocation_result: parse_json_optional(row, 13)?,
        invocation_error: row.get(14)?,
        created_at: parse_time(row, 15)?,
        decided_at: parse_time_optional(row, 16)?,
        invoked_at: parse_time_optional(row, 17)?,
    })
}

fn parse_json(row: &Row<'_>, index: usize) -> rusqlite::Result<serde_json::Value> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_json_optional(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<serde_json::Value>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn create(
    repository: &RunRepository,
    new: NewAssistantConversation,
) -> Result<AssistantConversation> {
    if new.title.trim().is_empty() {
        return Err(HarnessError::InvalidConversationRequest(
            "Conversation title must not be empty.".into(),
        ));
    }
    if new.model.trim().is_empty() {
        return Err(HarnessError::AssistantNotConfigured(
            "no text model is selected".into(),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let now = timestamp();
    repository.guarded(|connection| {
        connection.execute(
            "INSERT INTO assistant_conversations (
                id, title, created_at, updated_at, model,
                system_prompt_template, system_prompt_sha256, system_prompt
             ) VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                new.title,
                now,
                new.model,
                new.system_prompt_template,
                new.system_prompt_sha256,
                new.system_prompt,
            ],
        )?;
        load_conversation(connection, &id)
    })
}

fn list(
    repository: &RunRepository,
    query: AssistantConversationListQuery,
) -> Result<AssistantConversationPage> {
    let limit = bounded_limit(query.limit, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, "Limit")?;
    let cursor = query
        .cursor
        .as_deref()
        .map(ConversationCursor::decode)
        .transpose()?;
    repository.guarded_read(|connection| {
        let mut statement = connection.prepare(
            "SELECT id, title, created_at, updated_at, model,
                    system_prompt_template, system_prompt_sha256, system_prompt
             FROM assistant_conversations
             WHERE (?1 IS NULL AND ?2 IS NULL) OR (updated_at, id) < (?1, ?2)
             ORDER BY updated_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![
                    cursor.as_ref().map(|value| &value.0),
                    cursor.as_ref().map(|value| &value.1),
                    i64::try_from(limit + 1).expect("bounded limit fits i64"),
                ],
                map_conversation,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut conversations = rows;
        let next_cursor = if conversations.len() > limit {
            conversations.pop();
            conversations
                .last()
                .map(|conversation| {
                    ConversationCursor(
                        conversation
                            .updated_at
                            .to_rfc3339_opts(SecondsFormat::Nanos, true),
                        conversation.id.clone(),
                    )
                    .encode()
                })
                .transpose()?
        } else {
            None
        };
        Ok(AssistantConversationPage {
            conversations,
            next_cursor,
        })
    })
}

fn get(
    repository: &RunRepository,
    conversation_id: &str,
    message_limit: Option<usize>,
) -> Result<AssistantConversationDetail> {
    let limit = bounded_limit(
        message_limit,
        DEFAULT_MESSAGE_LIMIT,
        MAX_MESSAGE_LIMIT,
        "Message limit",
    )?;
    repository.guarded_read(|connection| {
        let conversation = load_conversation(connection, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, seq, role, content_json, model,
                    status, error, tool_call_id, created_at
             FROM (
                 SELECT id, conversation_id, seq, role, content_json, model,
                    COALESCE((SELECT status FROM assistant_message_terminal_states WHERE message_id = assistant_messages.id), status) AS status, error,
                        tool_call_id, created_at
                 FROM assistant_messages
                 WHERE conversation_id = ?1
                 ORDER BY seq DESC
                 LIMIT ?2
             )
             ORDER BY seq ASC",
        )?;
        let messages = statement
            .query_map(
                params![
                    conversation_id,
                    i64::try_from(limit).expect("bounded limit fits i64")
                ],
                map_message,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(AssistantConversationDetail {
            conversation,
            messages,
        })
    })
}

fn append_message(
    repository: &RunRepository,
    conversation_id: &str,
    message: NewAssistantMessage,
) -> Result<AssistantMessage> {
    validate_message(&message)?;
    let id = Uuid::new_v4().to_string();
    let now = timestamp();
    let content_json = serde_json::to_string(&message.content)?;
    repository.guarded(|connection| {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id = ?1)",
            [conversation_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(HarnessError::ConversationNotFound {
                conversation_id: conversation_id.to_owned(),
            });
        }
        let seq: i64 = connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM assistant_messages WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO assistant_messages (
                id, conversation_id, seq, role, content_json, model, status, error,
                tool_call_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                conversation_id,
                seq,
                message.role.as_str(),
                content_json,
                message.model,
                message.status.as_str(),
                message.error,
                message.tool_call_id,
                now,
            ],
        )?;
        connection.execute(
            "UPDATE assistant_conversations SET updated_at = ?2 WHERE id = ?1",
            params![conversation_id, now],
        )?;
        load_message(connection, &id)
    })
}

fn validate_message(message: &NewAssistantMessage) -> Result<()> {
    let matches = matches!(
        (&message.role, &message.content),
        (AssistantMessageRole::User, ChatMessage::User(_))
            | (
                AssistantMessageRole::Assistant,
                ChatMessage::Assistant { .. }
            )
            | (AssistantMessageRole::Tool, ChatMessage::Tool { .. })
    );
    if !matches {
        return Err(HarnessError::InvalidConversationRequest(
            "Message role and content do not match.".into(),
        ));
    }
    if message.status == AssistantMessageStatus::Failed && message.error.is_none() {
        return Err(HarnessError::InvalidConversationRequest(
            "A failed message must include an error.".into(),
        ));
    }
    Ok(())
}

fn load_conversation(
    connection: &rusqlite::Connection,
    conversation_id: &str,
) -> Result<AssistantConversation> {
    connection
        .query_row(
            "SELECT id, title, created_at, updated_at, model,
                    system_prompt_template, system_prompt_sha256, system_prompt
             FROM assistant_conversations WHERE id = ?1",
            [conversation_id],
            map_conversation,
        )
        .optional()?
        .ok_or_else(|| HarnessError::ConversationNotFound {
            conversation_id: conversation_id.to_owned(),
        })
}

fn load_message(connection: &rusqlite::Connection, message_id: &str) -> Result<AssistantMessage> {
    connection
        .query_row(
            "SELECT id, conversation_id, seq, role, content_json, model,
                    COALESCE((SELECT status FROM assistant_message_terminal_states WHERE message_id = assistant_messages.id), status) AS status, error,
                tool_call_id, created_at
         FROM assistant_messages WHERE id = ?1",
            [message_id],
            map_message,
        )
        .map_err(Into::into)
}

fn map_conversation(row: &Row<'_>) -> rusqlite::Result<AssistantConversation> {
    Ok(AssistantConversation {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at: parse_time(row, 2)?,
        updated_at: parse_time(row, 3)?,
        model: row.get(4)?,
        system_prompt_template: row.get(5)?,
        system_prompt_sha256: row.get(6)?,
        system_prompt: row.get(7)?,
    })
}

fn map_message(row: &Row<'_>) -> rusqlite::Result<AssistantMessage> {
    let role = AssistantMessageRole::parse(&row.get::<_, String>(3)?)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let status = AssistantMessageStatus::parse(&row.get::<_, String>(6)?)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let content = serde_json::from_str(&row.get::<_, String>(4)?)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    Ok(AssistantMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        seq: row.get(2)?,
        role,
        content,
        model: row.get(5)?,
        status,
        error: row.get(7)?,
        tool_call_id: row.get(8)?,
        created_at: parse_time(row, 9)?,
    })
}

fn parse_time(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let value = row.get::<_, String>(index)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn parse_time_optional(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|time| time.with_timezone(&Utc))
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        index,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
        })
        .transpose()
}

fn bounded_limit(
    value: Option<usize>,
    default: usize,
    maximum: usize,
    label: &str,
) -> Result<usize> {
    let value = value.unwrap_or(default);
    if !(1..=maximum).contains(&value) {
        return Err(HarnessError::InvalidConversationRequest(format!(
            "{label} must be between 1 and {maximum}."
        )));
    }
    Ok(value)
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}
