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
    StartupPanic,
    PanicAfterDelta,
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
            Reply::StartupPanic => panic!("provider startup panicked"),
            Reply::PanicAfterDelta => {
                if self.requests.lock().unwrap().len() == 1 {
                    Ok(Box::pin(PanicStream { emitted: false }))
                } else {
                    Ok(Box::pin(stream::pending()))
                }
            }
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

struct PanicStream {
    emitted: bool,
}

impl Stream for PanicStream {
    type Item = Result<ChatDelta>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if !self.emitted {
            self.emitted = true;
            return Poll::Ready(Some(Ok(ChatDelta::Text("partial".into()))));
        }
        panic!("provider stream panicked")
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
async fn concurrent_first_load_creates_one_complete_template() {
    let root = workspace();
    let templates = PromptTemplates::new(root.path().join("data"));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let templates = templates.clone();
        tasks.push(tokio::spawn(async move { templates.load("system").await }));
    }
    let mut loaded = Vec::new();
    for task in tasks {
        loaded.push(task.await.unwrap().unwrap());
    }
    assert!(loaded.iter().all(|template| template == &loaded[0]));
    assert!(loaded[0].content.contains("local GenAImage assistant"));
}

#[tokio::test]
async fn dropping_send_during_setup_releases_the_in_flight_lease() {
    let root = workspace();
    let database = root.path().join("state.sqlite3");
    let store = ConversationStore::open(&database).await.unwrap();
    let service = ConversationService::new(
        store,
        PromptTemplates::new(root.path().join("data")),
        FakeLlm::new(Reply::Pending),
        "text-model",
    )
    .unwrap();
    let conversation = service
        .create_conversation("Setup drop".into())
        .await
        .unwrap();

    let mut writer = rusqlite::Connection::open(&database).unwrap();
    writer.busy_timeout(Duration::from_secs(10)).unwrap();
    let (append_started, append_gate) = service.test_append_controls();
    let append_permit = append_gate.acquire().await.unwrap();
    let task_service = service.clone();
    let id = conversation.id.clone();
    let task = tokio::spawn(async move { task_service.send_message(&id, user("first")).await });
    timeout(Duration::from_secs(1), append_started.notified())
        .await
        .unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute_batch("UPDATE assistant_conversations SET title = title;")
        .unwrap();
    drop(append_permit);
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await;
    assert!(service.has_in_flight(&conversation.id));
    let interleaved = service
        .send_message(&conversation.id, user("interleaved"))
        .await
        .err();
    assert!(
        matches!(interleaved, Some(HarnessError::ConversationConflict(_))),
        "unexpected interleaved outcome: {interleaved:?}"
    );

    drop(transaction);
    timeout(Duration::from_secs(1), async {
        while service.has_in_flight(&conversation.id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert!(matches!(
        detail.messages[0].content,
        crate::ChatMessage::User(_)
    ));
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Cancelled);

    let _stream = service
        .send_message(&conversation.id, user("second"))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), service.cancel(&conversation.id))
        .await
        .unwrap()
        .unwrap();
    service.delete_conversation(&conversation.id).await.unwrap();
}

#[tokio::test]
async fn provider_startup_panic_is_persisted_and_forwarded() {
    let (_root, service, _provider) = fixture(Reply::StartupPanic).await;
    let conversation = service
        .create_conversation("Startup panic".into())
        .await
        .unwrap();
    let mut response = service
        .send_message(&conversation.id, user("panic"))
        .await
        .unwrap();
    let error = response.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("provider startup panicked"));
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Failed);
    assert!(detail.messages[1]
        .error
        .as_deref()
        .is_some_and(|reason| reason.contains("provider startup panicked")));
}

#[tokio::test]
async fn provider_stream_panic_releases_the_in_flight_lease() {
    let (_root, service, _provider) = fixture(Reply::PanicAfterDelta).await;
    let conversation = service.create_conversation("Panic".into()).await.unwrap();
    let mut response = service
        .send_message(&conversation.id, user("panic"))
        .await
        .unwrap();
    assert!(matches!(
        response.next().await.unwrap().unwrap(),
        ChatDelta::Text(text) if text == "partial"
    ));
    let error = timeout(Duration::from_secs(1), response.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("provider stream panicked"));
    assert!(!service.has_in_flight(&conversation.id));
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Failed);
    assert!(matches!(
        &detail.messages[1].content,
        crate::ChatMessage::Assistant { text, .. } if text == "partial"
    ));

    let _retry = service
        .send_message(&conversation.id, user("retry"))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), service.cancel(&conversation.id))
        .await
        .unwrap()
        .unwrap();
    service.delete_conversation(&conversation.id).await.unwrap();
}

#[tokio::test]
async fn cancellation_is_not_blocked_by_a_full_output_channel() {
    let parts = (0..100).map(|value| value.to_string()).collect();
    let (_root, service, _provider) = fixture(Reply::Complete(parts)).await;
    let conversation = service
        .create_conversation("Full stream".into())
        .await
        .unwrap();
    let _response = service
        .send_message(&conversation.id, user("start"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    timeout(Duration::from_secs(1), service.cancel(&conversation.id))
        .await
        .unwrap()
        .unwrap();
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
