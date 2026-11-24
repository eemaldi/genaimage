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
            ChatDelta::ToolResult { .. } | ChatDelta::ConfirmationRequired { .. } => {
                panic!("unexpected runtime event")
            }
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

#[derive(Clone)]
struct FakeTool {
    name: &'static str,
    schema: serde_json::Value,
    confirmation: bool,
    calls: Arc<Mutex<Vec<(crate::ToolContext, serde_json::Value)>>>,
    result: serde_json::Value,
}

#[async_trait]
impl crate::AssistantTool for FakeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn json_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }
    fn requires_confirmation(&self) -> bool {
        self.confirmation
    }
    async fn invoke(
        &self,
        context: crate::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.calls.lock().unwrap().push((context, arguments));
        Ok(self.result.clone())
    }
}

struct ScriptedLlm {
    scripts: Mutex<std::collections::VecDeque<Vec<ChatDelta>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        self.requests.lock().unwrap().push(request);
        let script = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }
    async fn list_models(&self) -> Result<Vec<LlmModel>> {
        Ok(vec![LlmModel {
            id: "text-model".into(),
            supports_tools: true,
            supports_vision: false,
        }])
    }
}

async fn runtime_fixture(
    scripts: Vec<Vec<ChatDelta>>,
    tools: Vec<Arc<dyn crate::AssistantTool>>,
) -> (TempDir, ConversationService, Arc<ScriptedLlm>) {
    let root = workspace();
    let store = ConversationStore::open(&root.path().join("state.sqlite3"))
        .await
        .unwrap();
    let llm = Arc::new(ScriptedLlm {
        scripts: Mutex::new(scripts.into()),
        requests: Mutex::new(Vec::new()),
    });
    let service = ConversationService::new(
        store,
        PromptTemplates::new(root.path().join("data")),
        llm.clone(),
        "text-model",
    )
    .unwrap()
    .with_assistant_tools(tools)
    .unwrap();
    (root, service, llm)
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ChatDelta {
    ChatDelta::ToolCall(crate::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    })
}

fn object_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","required":["value"],"properties":{"value":{"type":"integer"}},"additionalProperties":false})
}

async fn collect(mut stream: crate::assistant::AssistantStream) -> Vec<ChatDelta> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

type FakeToolCalls = Arc<Mutex<Vec<(crate::ToolContext, serde_json::Value)>>>;

fn fake_tool(
    name: &'static str,
    confirmation: bool,
) -> (Arc<dyn crate::AssistantTool>, FakeToolCalls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(FakeTool {
        name,
        schema: object_schema(),
        confirmation,
        calls: calls.clone(),
        result: serde_json::json!({"answer":42}),
    });
    (tool, calls)
}

#[tokio::test]
async fn executes_a_multi_step_tool_loop_and_audits_each_call() {
    let (tool, calls) = fake_tool("calculate", false);
    let (_root, service, llm) = runtime_fixture(
        vec![
            vec![tool_call(
                "call-1",
                "calculate",
                serde_json::json!({"value":4}),
            )],
            vec![ChatDelta::Text("done".into())],
        ],
        vec![tool],
    )
    .await;
    let conversation = service.create_conversation("Tools".into()).await.unwrap();
    let events = collect(
        service
            .send_message(&conversation.id, user("go"))
            .await
            .unwrap(),
    )
    .await;
    assert!(events
        .iter()
        .any(|event| matches!(event, ChatDelta::ToolResult { ok: true, .. })));
    assert_eq!(calls.lock().unwrap().len(), 1);
    {
        let requests = llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(
        |message| matches!(message, crate::ChatMessage::Tool { call_id, .. } if call_id == "call-1")
        ));
    }
    assert_eq!(
        service
            .list_tool_audit(&conversation.id, None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_and_unknown_calls_are_returned_to_model_without_invocation() {
    let (tool, calls) = fake_tool("calculate", false);
    let (_root, service, llm) = runtime_fixture(
        vec![
            vec![
                tool_call("bad", "calculate", serde_json::json!({"value":"x"})),
                tool_call("unknown", "missing", serde_json::json!({})),
            ],
            vec![ChatDelta::Text("handled".into())],
        ],
        vec![tool],
    )
    .await;
    let conversation = service.create_conversation("Errors".into()).await.unwrap();
    collect(
        service
            .send_message(&conversation.id, user("go"))
            .await
            .unwrap(),
    )
    .await;
    assert!(calls.lock().unwrap().is_empty());
    {
        let requests = llm.requests.lock().unwrap();
        let tool_errors = requests[1].messages.iter().filter(|message| matches!(message, crate::ChatMessage::Tool { result, .. } if result.get("ok") == Some(&serde_json::json!(false)))).count();
        assert_eq!(tool_errors, 2);
    }
    let audit = service
        .list_tool_audit(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(audit.len(), 2);
    assert!(audit.iter().any(|row| row.validation_outcome == "invalid"));
    assert!(audit
        .iter()
        .any(|row| row.validation_outcome == "unknown_tool"));
}

#[tokio::test]
async fn loop_guard_persists_a_failed_terminal_state() {
    let (tool, _calls) = fake_tool("calculate", false);
    let scripts = (0..3)
        .map(|index| {
            vec![tool_call(
                &format!("call-{index}"),
                "calculate",
                serde_json::json!({"value":index}),
            )]
        })
        .collect();
    let (_root, service, _) = runtime_fixture(scripts, vec![tool]).await;
    let conversation = service.create_conversation("Loop".into()).await.unwrap();
    let service = service.with_max_tool_steps(2).unwrap();
    let stream = service
        .send_message(&conversation.id, user("loop"))
        .await
        .unwrap();
    collect(stream).await;
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(
        detail.messages.last().unwrap().status,
        AssistantMessageStatus::Failed
    );
    assert!(detail
        .messages
        .last()
        .unwrap()
        .error
        .as_deref()
        .unwrap()
        .contains("maximum of 2"));
}

#[tokio::test]
async fn confirmation_pause_confirm_and_double_decision_are_enforced() {
    let (tool, calls) = fake_tool("submit", true);
    let (_root, service, _) = runtime_fixture(
        vec![
            vec![tool_call(
                "confirmed",
                "submit",
                serde_json::json!({"value":7}),
            )],
            vec![ChatDelta::Text("submitted".into())],
        ],
        vec![tool],
    )
    .await;
    let conversation = service.create_conversation("Confirm".into()).await.unwrap();
    let paused = collect(
        service
            .send_message(&conversation.id, user("submit"))
            .await
            .unwrap(),
    )
    .await;
    let confirmation_id = paused
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        service
            .get_conversation(&conversation.id, None)
            .await
            .unwrap()
            .messages
            .last()
            .unwrap()
            .status,
        AssistantMessageStatus::AwaitingConfirmation
    );
    collect(
        service
            .decide_confirmation(
                &conversation.id,
                &confirmation_id,
                crate::assistant::AssistantConfirmationDecision::Confirm,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    {
        let recorded_calls = calls.lock().unwrap();
        let recorded = &recorded_calls[0];
        assert_eq!(
            recorded.0.confirmation_id.as_deref(),
            Some(confirmation_id.as_str())
        );
        assert_eq!(recorded.1, serde_json::json!({"value":7}));
    }
    let duplicate = service
        .decide_confirmation(
            &conversation.id,
            &confirmation_id,
            crate::assistant::AssistantConfirmationDecision::Confirm,
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        duplicate,
        HarnessError::ToolConfirmationConflict(_)
    ));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn edit_validates_arguments_and_reject_does_not_invoke() {
    let (tool, calls) = fake_tool("submit", true);
    let (_root, service, _) = runtime_fixture(
        vec![
            vec![tool_call("edit", "submit", serde_json::json!({"value":1}))],
            vec![ChatDelta::Text("edited".into())],
        ],
        vec![tool],
    )
    .await;
    let conversation = service.create_conversation("Edit".into()).await.unwrap();
    let events = collect(
        service
            .send_message(&conversation.id, user("edit"))
            .await
            .unwrap(),
    )
    .await;
    let id = events
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();
    let invalid = service
        .decide_confirmation(
            &conversation.id,
            &id,
            crate::assistant::AssistantConfirmationDecision::Edit(
                serde_json::json!({"value":"bad"}),
            ),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(invalid, HarnessError::InvalidToolArguments(_)));
    assert!(calls.lock().unwrap().is_empty());
    collect(
        service
            .decide_confirmation(
                &conversation.id,
                &id,
                crate::assistant::AssistantConfirmationDecision::Edit(
                    serde_json::json!({"value":9}),
                ),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(calls.lock().unwrap()[0].1, serde_json::json!({"value":9}));

    let (reject_tool, reject_calls) = fake_tool("rejectable", true);
    let (_root2, reject_service, _) = runtime_fixture(
        vec![
            vec![tool_call(
                "reject",
                "rejectable",
                serde_json::json!({"value":2}),
            )],
            vec![ChatDelta::Text("rejected".into())],
        ],
        vec![reject_tool],
    )
    .await;
    let rejected_conversation = reject_service
        .create_conversation("Reject".into())
        .await
        .unwrap();
    let events = collect(
        reject_service
            .send_message(&rejected_conversation.id, user("reject"))
            .await
            .unwrap(),
    )
    .await;
    let id = events
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();
    collect(
        reject_service
            .decide_confirmation(
                &rejected_conversation.id,
                &id,
                crate::assistant::AssistantConfirmationDecision::Reject(Some("not now".into())),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(reject_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_during_tool_invocation_is_terminal() {
    #[derive(Clone)]
    struct BlockingTool;
    #[async_trait]
    impl crate::AssistantTool for BlockingTool {
        fn name(&self) -> &str {
            "block"
        }
        fn description(&self) -> &str {
            "block"
        }
        fn json_schema(&self) -> serde_json::Value {
            object_schema()
        }
        fn requires_confirmation(&self) -> bool {
            false
        }
        async fn invoke(
            &self,
            _context: crate::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<serde_json::Value> {
            futures_util::future::pending().await
        }
    }
    let (_root, service, _) = runtime_fixture(
        vec![vec![tool_call(
            "block",
            "block",
            serde_json::json!({"value":1}),
        )]],
        vec![Arc::new(BlockingTool)],
    )
    .await;
    let conversation = service
        .create_conversation("Cancel tool".into())
        .await
        .unwrap();
    let mut stream = service
        .send_message(&conversation.id, user("go"))
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ChatDelta::ToolCall(_)
    ));
    timeout(Duration::from_secs(1), service.cancel(&conversation.id))
        .await
        .unwrap()
        .unwrap();
    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    assert_eq!(
        detail.messages.last().unwrap().status,
        AssistantMessageStatus::Cancelled
    );
}

#[tokio::test]
async fn a_model_without_tool_support_fails_without_dropping_the_tool_list() {
    use crate::config::LlmConfig;
    use crate::LlmRoles;

    let root = workspace();
    let store = ConversationStore::open(&root.path().join("state.sqlite3"))
        .await
        .unwrap();
    let provider = Arc::new(ScriptedLlm {
        scripts: Mutex::new(std::collections::VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    struct NoTools(Arc<ScriptedLlm>);
    #[async_trait]
    impl LlmProvider for NoTools {
        async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
            self.0.chat_stream(request).await
        }
        async fn list_models(&self) -> Result<Vec<LlmModel>> {
            Ok(vec![LlmModel {
                id: "text-model".into(),
                supports_tools: false,
                supports_vision: false,
            }])
        }
    }
    let provider: Arc<dyn LlmProvider> = Arc::new(NoTools(provider.clone()));
    let config = LlmConfig {
        base_url: "http://127.0.0.1:1".into(),
        text_model: "text-model".into(),
        ..LlmConfig::default()
    };
    let binding = LlmRoles::new(&config, Some(provider.clone()))
        .text()
        .await
        .unwrap();
    let (tool, calls) = fake_tool("requires_tools", false);
    let service =
        ConversationService::unbound(store, PromptTemplates::new(root.path().join("data")))
            .with_assistant_tools(vec![tool])
            .unwrap();
    let conversation = service
        .create_conversation_with_model("No tools".into(), "text-model".into())
        .await
        .unwrap();
    let error = service
        .send_message_with_binding(&conversation.id, user("go"), binding)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("text-model"));
    assert!(error.to_string().contains("tools"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_model_that_loses_tool_support_cannot_claim_confirmation() {
    use crate::{config::LlmConfig, LlmRoles};

    let (tool, calls) = fake_tool("requires_tools", true);
    let (_root, service, provider) = runtime_fixture(
        vec![vec![tool_call(
            "confirm-lost-tools",
            "requires_tools",
            serde_json::json!({"value":7}),
        )]],
        vec![tool],
    )
    .await;
    let conversation = service
        .create_conversation("Lost tools".into())
        .await
        .unwrap();
    let events = collect(
        service
            .send_message(&conversation.id, user("submit"))
            .await
            .unwrap(),
    )
    .await;
    let confirmation_id = events
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();

    struct NoTools(Arc<ScriptedLlm>);
    #[async_trait]
    impl LlmProvider for NoTools {
        async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
            self.0.chat_stream(request).await
        }
        async fn list_models(&self) -> Result<Vec<LlmModel>> {
            Ok(vec![LlmModel {
                id: "text-model".into(),
                supports_tools: false,
                supports_vision: false,
            }])
        }
    }
    let no_tools: Arc<dyn LlmProvider> = Arc::new(NoTools(provider));
    let config = LlmConfig {
        base_url: "http://127.0.0.1:1".into(),
        text_model: "text-model".into(),
        ..LlmConfig::default()
    };
    let binding = LlmRoles::new(&config, Some(no_tools)).text().await.unwrap();
    let error = service
        .decide_confirmation_with_binding(
            &conversation.id,
            &confirmation_id,
            crate::assistant::AssistantConfirmationDecision::Confirm,
            binding,
        )
        .await
        .err()
        .expect("unsupported model must fail before claiming");
    assert!(error.to_string().contains("text-model"));
    assert!(error.to_string().contains("tools"));
    assert!(calls.lock().unwrap().is_empty());
    let pending = service
        .pending_confirmation(&conversation.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.id, confirmation_id);
    assert_eq!(
        pending.status,
        crate::assistant::AssistantToolConfirmationStatus::Pending
    );
}

#[tokio::test]
async fn a_crash_after_claim_recovers_as_outcome_unknown_without_reinvocation() {
    let (tool, calls) = fake_tool("durable", true);
    let (_root, service, _) = runtime_fixture(
        vec![vec![tool_call(
            "crash",
            "durable",
            serde_json::json!({"value":1}),
        )]],
        vec![tool],
    )
    .await;
    let conversation = service
        .create_conversation("Crash recovery".into())
        .await
        .unwrap();
    let events = collect(
        service
            .send_message(&conversation.id, user("submit"))
            .await
            .unwrap(),
    )
    .await;
    let confirmation_id = events
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();
    let claimed = service
        .test_claim_confirmation(&conversation.id, &confirmation_id)
        .await
        .unwrap();
    assert_eq!(
        claimed.status,
        crate::assistant::AssistantToolConfirmationStatus::Invoking
    );

    let detail = service
        .get_conversation(&conversation.id, None)
        .await
        .unwrap();
    let tool_message = detail.messages.last().unwrap();
    assert!(matches!(
        &tool_message.content,
        crate::ChatMessage::Tool { result, .. }
            if result["ok"] == false && result["error"].as_str().unwrap().contains("Outcome unknown")
    ));
    assert!(calls.lock().unwrap().is_empty());
    let audit = service
        .list_tool_audit(&conversation.id, None)
        .await
        .unwrap();
    assert!(audit.iter().any(|row| {
        row.decision == "outcome_unknown"
            && row.confirmation_status.as_deref() == Some("outcome_unknown")
    }));
    let retry = service
        .decide_confirmation(
            &conversation.id,
            &confirmation_id,
            crate::assistant::AssistantConfirmationDecision::Confirm,
        )
        .await
        .err()
        .expect("outcome-unknown confirmations must not be replayed");
    assert!(matches!(retry, HarnessError::ToolConfirmationConflict(_)));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_during_confirmed_invocation_still_records_the_outcome() {
    struct CompletingTool {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl crate::AssistantTool for CompletingTool {
        fn name(&self) -> &str {
            "complete_after_cancel"
        }
        fn description(&self) -> &str {
            "completes after cancellation"
        }
        fn json_schema(&self) -> serde_json::Value {
            object_schema()
        }
        fn requires_confirmation(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _context: crate::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(serde_json::json!({"completed":true}))
        }
    }
    let tool = Arc::new(CompletingTool {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (_root, service, _) = runtime_fixture(
        vec![vec![tool_call(
            "cancel-confirmed",
            "complete_after_cancel",
            serde_json::json!({"value":1}),
        )]],
        vec![tool.clone()],
    )
    .await;
    let conversation = service
        .create_conversation("Cancel confirmed".into())
        .await
        .unwrap();
    let events = collect(
        service
            .send_message(&conversation.id, user("submit"))
            .await
            .unwrap(),
    )
    .await;
    let confirmation_id = events
        .iter()
        .find_map(|event| match event {
            ChatDelta::ConfirmationRequired {
                confirmation_id, ..
            } => Some(confirmation_id.clone()),
            _ => None,
        })
        .unwrap();
    let _stream = service
        .decide_confirmation(
            &conversation.id,
            &confirmation_id,
            crate::assistant::AssistantConfirmationDecision::Confirm,
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(1), tool.started.notified())
        .await
        .unwrap();
    let cancel_service = service.clone();
    let cancel_id = conversation.id.clone();
    let cancel = tokio::spawn(async move { cancel_service.cancel(&cancel_id).await });
    tokio::task::yield_now().await;
    tool.release.notify_one();
    timeout(Duration::from_secs(1), cancel)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(tool.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let audit = service
        .list_tool_audit(&conversation.id, None)
        .await
        .unwrap();
    assert!(audit.iter().any(|row| {
        row.decision == "invocation_succeeded"
            && row.confirmation_status.as_deref() == Some("succeeded")
            && row.invocation_result == Some(serde_json::json!({"completed":true}))
    }));
}
