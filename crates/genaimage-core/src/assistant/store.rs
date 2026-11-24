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
}

impl AssistantMessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
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
            "SELECT id, conversation_id, seq, role, content_json, model, status, error,
                    tool_call_id, created_at
             FROM (
                 SELECT id, conversation_id, seq, role, content_json, model, status, error,
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
            "SELECT id, conversation_id, seq, role, content_json, model, status, error,
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
