use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use async_trait::async_trait;
use futures_util::{stream, Stream, StreamExt};
use tempfile::TempDir;
use tokio::time::{timeout, Duration};

use crate::{
    assistant::{
        AssistantConversationListQuery, AssistantMessageStatus, ConversationService,
        ConversationStore, PromptTemplates,
    },
    ChatContent, ChatDelta, ChatRequest, ChatStream, HarnessError, LlmModel, LlmProvider, Result,
};

#[derive(Clone)]
enum Reply {
    Complete(Vec<String>),
    FirstThenPending(String),
    Pending,
    ErrorAfter(String),
    StartupError,
}

struct FakeLlm {
    reply: Reply,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeLlm {
    fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            reply,
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl LlmProvider for FakeLlm {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        self.requests.lock().unwrap().push(request);
        match &self.reply {
            Reply::Complete(parts) => Ok(Box::pin(stream::iter(
                parts.clone().into_iter().map(ChatDelta::Text).map(Ok),
            ))),
            Reply::FirstThenPending(first) => Ok(Box::pin(FirstThenPending {
                first: Some(first.clone()),
            })),
            Reply::Pending => Ok(Box::pin(FirstThenPending { first: None })),
            Reply::ErrorAfter(text) => Ok(Box::pin(stream::iter([
                Ok(ChatDelta::Text(text.clone())),
                Err(HarnessError::Configuration("provider stream failed".into())),
            ]))),
            Reply::StartupError => Err(HarnessError::Configuration("provider unavailable".into())),
        }
    }

    async fn list_models(&self) -> Result<Vec<LlmModel>> {
        Ok(vec![LlmModel {
            id: "text-model".into(),
            supports_tools: false,
            supports_vision: false,
        }])
    }
}

struct FirstThenPending {
    first: Option<String>,
}

impl Stream for FirstThenPending {
    type Item = Result<ChatDelta>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.first.take() {
            Some(text) => Poll::Ready(Some(Ok(ChatDelta::Text(text)))),
            None => Poll::Pending,
        }
    }
}

fn workspace() -> TempDir {
    tempfile::Builder::new()
        .prefix(".assistant-test-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap()
}

async fn fixture(reply: Reply) -> (TempDir, ConversationService, Arc<FakeLlm>) {
    let root = workspace();
    let store = ConversationStore::open(&root.path().join("state.sqlite3"))
        .await
        .unwrap();
    let provider = FakeLlm::new(reply);
    let service = ConversationService::new(
        store,
        PromptTemplates::new(root.path().join("data")),
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        "text-model",
    )
    .unwrap();
    (root, service, provider)
}

fn user(text: &str) -> Vec<ChatContent> {
    vec![ChatContent::Text(text.into())]
}

#[tokio::test]
async fn streams_deltas_and_persists_the_completed_turn() {
    let (_root, service, provider) =
        fixture(Reply::Complete(vec!["Hello".into(), " world".into()])).await;
    let conversation = service
        .create_conversation("Greeting".into())
        .await
        .unwrap();
    let mut response = service
        .send_message(&conversation.id, user("Hello?"))
        .await
        .unwrap();
    let mut parts = Vec::new();
    while let Some(delta) = response.next().await {
        match delta.unwrap() {
            ChatDelta::Text(text) => parts.push(text),
            ChatDelta::ToolCall(_) => panic!("unexpected tool call"),
        }
    }

    assert_eq!(parts, ["Hello", " world"]);
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Complete);
    assert!(matches!(
        &detail.messages[1].content,
        crate::ChatMessage::Assistant { text, tool_calls }
            if text == "Hello world" && tool_calls.is_empty()
    ));
    let request = &provider.requests.lock().unwrap()[0];
    assert_eq!(request.model, "text-model");
    assert!(matches!(
        &request.messages[0],
        crate::ChatMessage::System(text) if text.contains("local GenAImage assistant")
    ));
}

#[tokio::test]
async fn cancellation_stops_promptly_and_keeps_partial_content() {
    let (_root, service, _provider) = fixture(Reply::FirstThenPending("partial".into())).await;
    let conversation = service.create_conversation("Cancel".into()).await.unwrap();
    let mut response = service
        .send_message(&conversation.id, user("Keep going"))
        .await
        .unwrap();
    assert!(matches!(
        response.next().await.unwrap().unwrap(),
        ChatDelta::Text(text) if text == "partial"
    ));
    timeout(Duration::from_secs(1), service.cancel(&conversation.id))
        .await
        .unwrap()
        .unwrap();
    assert!(response.next().await.is_none());

    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Cancelled);
    assert!(matches!(
        &detail.messages[1].content,
        crate::ChatMessage::Assistant { text, .. } if text == "partial"
    ));
}

#[tokio::test]
async fn rejects_a_second_in_flight_send_without_appending_another_user_message() {
    let (_root, service, _provider) = fixture(Reply::Pending).await;
    let conversation = service
        .create_conversation("One at a time".into())
        .await
        .unwrap();
    let _response = service
        .send_message(&conversation.id, user("first"))
        .await
        .unwrap();
    let error = service
        .send_message(&conversation.id, user("second"))
        .await
        .err()
        .expect("second send should conflict");
    assert!(matches!(error, HarnessError::ConversationConflict(_)));
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 1);
    service.cancel(&conversation.id).await.unwrap();
}

#[tokio::test]
async fn provider_stream_errors_are_persisted_and_forwarded() {
    let (_root, service, _provider) = fixture(Reply::ErrorAfter("partial".into())).await;
    let conversation = service.create_conversation("Failure".into()).await.unwrap();
    let mut response = service
        .send_message(&conversation.id, user("fail"))
        .await
        .unwrap();
    assert!(matches!(
        response.next().await.unwrap().unwrap(),
        ChatDelta::Text(text) if text == "partial"
    ));
    let error = response.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("provider stream failed"));
    assert!(response.next().await.is_none());

    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Failed);
    assert!(detail.messages[1]
        .error
        .as_deref()
        .is_some_and(|reason| reason.contains("provider stream failed")));
}

#[tokio::test]
async fn startup_provider_errors_are_persisted_and_forwarded() {
    let (_root, service, _provider) = fixture(Reply::StartupError).await;
    let conversation = service.create_conversation("Failure".into()).await.unwrap();
    let mut response = service
        .send_message(&conversation.id, user("fail"))
        .await
        .unwrap();
    let error = response.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("provider unavailable"));
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Failed);
}

#[tokio::test]
async fn prompt_templates_are_created_once_and_user_edits_are_preserved() {
    let root = workspace();
    let templates = PromptTemplates::new(root.path().join("data"));
    let first = templates.load("system").await.unwrap();
    assert!(first.content.contains("local GenAImage assistant"));
    let edited = "A user-edited prompt.\n";
    std::fs::write(root.path().join("data/assistant/prompts/system.md"), edited).unwrap();
    let second = templates.load("system").await.unwrap();
    assert_eq!(second.content, edited);
    use sha2::{Digest, Sha256};
    assert_eq!(
        second.sha256,
        hex::encode(Sha256::digest(edited.as_bytes()))
    );
}

#[tokio::test]
async fn an_explicit_model_is_required() {
    let root = workspace();
    let store = ConversationStore::open(&root.path().join("state.sqlite3"))
        .await
        .unwrap();
    let result = ConversationService::new(
        store,
        PromptTemplates::new(root.path().join("data")),
        FakeLlm::new(Reply::Pending),
        "",
    );
    assert!(matches!(
        result,
        Err(HarnessError::AssistantNotConfigured(_))
    ));
}

#[tokio::test]
async fn conversation_pagination_and_message_limits_are_bounded() {
    let (_root, service, _provider) = fixture(Reply::Pending).await;
    for title in ["One", "Two", "Three"] {
        service.create_conversation(title.into()).await.unwrap();
    }
    let first = service
        .list_conversations(AssistantConversationListQuery {
            cursor: None,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(first.conversations.len(), 2);
    assert_eq!(first.conversations[0].title, "Three");
    let cursor = first
        .next_cursor
        .expect("three conversations have a second page");
    let second = service
        .list_conversations(AssistantConversationListQuery {
            cursor: Some(cursor),
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(second.conversations.len(), 1);
    assert!(second.next_cursor.is_none());
    let ids: Vec<_> = first
        .conversations
        .iter()
        .chain(&second.conversations)
        .map(|conversation| conversation.id.as_str())
        .collect();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    assert!(matches!(
        service
            .list_conversations(AssistantConversationListQuery {
                cursor: None,
                limit: Some(0),
            })
            .await,
        Err(HarnessError::InvalidConversationRequest(_))
    ));
    assert!(matches!(
        service
            .get_conversation(&first.conversations[0].id, Some(501))
            .await,
        Err(HarnessError::InvalidConversationRequest(_))
    ));
}
