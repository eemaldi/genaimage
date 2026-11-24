use std::{collections::HashMap, pin::Pin, sync::Arc, task::Poll};

use futures_util::StreamExt;
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{ChatContent, ChatDelta, ChatMessage, ChatRequest, HarnessError, LlmProvider, Result};

use super::{
    AssistantConversation, AssistantConversationDetail, AssistantConversationListQuery,
    AssistantConversationPage, AssistantMessageStatus, ConversationStore, NewAssistantConversation,
    NewAssistantMessage, PromptTemplates, SYSTEM_PROMPT_TEMPLATE,
};

const STREAM_CHANNEL_CAPACITY: usize = 32;

#[derive(Clone)]
struct InFlight {
    id: Uuid,
    cancellation: CancellationToken,
    finished: watch::Receiver<Option<std::result::Result<(), String>>>,
}

pub struct AssistantStream {
    receiver: mpsc::Receiver<Result<ChatDelta>>,
}

impl futures_core::Stream for AssistantStream {
    type Item = Result<ChatDelta>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

#[derive(Clone)]
pub struct ConversationService {
    store: ConversationStore,
    templates: PromptTemplates,
    provider: Arc<dyn LlmProvider>,
    model: String,
    in_flight: Arc<Mutex<HashMap<String, InFlight>>>,
}

impl ConversationService {
    pub fn new(
        store: ConversationStore,
        templates: PromptTemplates,
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let model = model.into();
        if model.trim().is_empty() {
            return Err(HarnessError::AssistantNotConfigured(
                "no text model is selected".into(),
            ));
        }
        Ok(Self {
            store,
            templates,
            provider,
            model,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn create_conversation(&self, title: String) -> Result<AssistantConversation> {
        let template = self.templates.load(SYSTEM_PROMPT_TEMPLATE).await?;
        self.store
            .create(NewAssistantConversation {
                title,
                model: self.model.clone(),
                system_prompt_template: template.name,
                system_prompt_sha256: template.sha256,
                system_prompt: template.content,
            })
            .await
    }

    pub async fn list_conversations(
        &self,
        query: AssistantConversationListQuery,
    ) -> Result<AssistantConversationPage> {
        self.store.list(query).await
    }

    pub async fn get_conversation(
        &self,
        conversation_id: &str,
        message_limit: Option<usize>,
    ) -> Result<AssistantConversationDetail> {
        self.store.get(conversation_id, message_limit).await
    }

    pub async fn append_message(
        &self,
        conversation_id: &str,
        message: NewAssistantMessage,
    ) -> Result<super::AssistantMessage> {
        self.store.append_message(conversation_id, message).await
    }

    pub async fn send_message(
        &self,
        conversation_id: &str,
        content: Vec<ChatContent>,
    ) -> Result<AssistantStream> {
        if content.is_empty() {
            return Err(HarnessError::InvalidConversationRequest(
                "User message content must not be empty.".into(),
            ));
        }
        let conversation = self.store.get(conversation_id, Some(1)).await?;
        if conversation.conversation.model != self.model {
            return Err(HarnessError::ConversationConflict(format!(
                "conversation '{}' is pinned to model '{}', not configured model '{}'",
                conversation_id, conversation.conversation.model, self.model
            )));
        }

        let cancellation = CancellationToken::new();
        let generation_id = Uuid::new_v4();
        let (finished_tx, finished_rx) = watch::channel(None);
        {
            let mut active = self.in_flight.lock().await;
            if active.contains_key(conversation_id) {
                return Err(HarnessError::ConversationConflict(format!(
                    "conversation '{conversation_id}' already has a generation in flight"
                )));
            }
            active.insert(
                conversation_id.to_owned(),
                InFlight {
                    id: generation_id,
                    cancellation: cancellation.clone(),
                    finished: finished_rx,
                },
            );
        }

        if let Err(error) = self
            .store
            .append_message(conversation_id, NewAssistantMessage::user(content))
            .await
        {
            self.finish_reservation(conversation_id, generation_id)
                .await;
            return Err(error);
        }
        let detail = match self.store.get(conversation_id, Some(500)).await {
            Ok(detail) => detail,
            Err(error) => {
                self.finish_reservation(conversation_id, generation_id)
                    .await;
                return Err(error);
            }
        };
        let mut messages = Vec::with_capacity(detail.messages.len() + 1);
        messages.push(ChatMessage::System(detail.conversation.system_prompt));
        messages.extend(detail.messages.into_iter().map(|message| message.content));
        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            tools: Vec::new(),
        };
        let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let reporter = sender.clone();
        let report_cancellation = cancellation.clone();
        let service = self.clone();
        let id = conversation_id.to_owned();
        tokio::spawn(async move {
            let result = run_generation(
                service.store.clone(),
                Arc::clone(&service.provider),
                service.model.clone(),
                &id,
                request,
                cancellation,
                sender,
            )
            .await;
            let completion = match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let message = error.to_string();
                    tokio::select! {
                        biased;
                        () = report_cancellation.cancelled() => {}
                        _ = reporter.closed() => {}
                        _ = reporter.send(Err(error)) => {}
                    }
                    Err(message)
                }
            };
            service.finish_reservation(&id, generation_id).await;
            let _ = finished_tx.send(Some(completion));
        });
        Ok(AssistantStream { receiver })
    }

    pub async fn cancel(&self, conversation_id: &str) -> Result<()> {
        let active = {
            let in_flight = self.in_flight.lock().await;
            in_flight.get(conversation_id).cloned()
        };
        let Some(mut active) = active else {
            self.store.get(conversation_id, Some(1)).await?;
            return Err(HarnessError::ConversationConflict(format!(
                "conversation '{conversation_id}' has no generation in flight"
            )));
        };
        active.cancellation.cancel();
        loop {
            if let Some(result) = active.finished.borrow().clone() {
                return result.map_err(HarnessError::Database);
            }
            active.finished.changed().await.map_err(|error| {
                HarnessError::Database(format!(
                    "assistant generation completion channel closed: {error}"
                ))
            })?;
        }
    }

    pub async fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
        loop {
            let active = self.in_flight.lock().await;
            if !active.contains_key(conversation_id) {
                return self.store.delete(conversation_id).await;
            }
            drop(active);
            self.cancel(conversation_id).await?;
        }
    }

    async fn finish_reservation(&self, conversation_id: &str, generation_id: Uuid) {
        let mut active = self.in_flight.lock().await;
        if active
            .get(conversation_id)
            .is_some_and(|generation| generation.id == generation_id)
        {
            active.remove(conversation_id);
        }
    }
}

async fn run_generation(
    store: ConversationStore,
    provider: Arc<dyn LlmProvider>,
    model: String,
    conversation_id: &str,
    request: ChatRequest,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
) -> Result<()> {
    let provider_stream = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            persist_terminal(&store, conversation_id, &model, String::new(), Vec::new(), AssistantMessageStatus::Cancelled, None).await?;
            return Ok(())
        }
        _ = sender.closed() => {
            persist_terminal(&store, conversation_id, &model, String::new(), Vec::new(), AssistantMessageStatus::Cancelled, None).await?;
            return Ok(())
        }
        result = provider.chat_stream(request) => result,
    };
    let mut stream = match provider_stream {
        Ok(stream) => stream,
        Err(error) => {
            let reason = error.to_string();
            persist_terminal(
                &store,
                conversation_id,
                &model,
                String::new(),
                Vec::new(),
                AssistantMessageStatus::Failed,
                Some(reason),
            )
            .await?;
            let _ = send_delta(&sender, &cancellation, Err(error)).await;
            return Ok(());
        }
    };
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                persist_terminal(&store, conversation_id, &model, text, tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(())
            }
            _ = sender.closed() => {
                persist_terminal(&store, conversation_id, &model, text, tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(())
            }
            item = stream.next() => match item {
                Some(Ok(delta)) => {
                    match &delta {
                        ChatDelta::Text(value) => text.push_str(value),
                        ChatDelta::ToolCall(call) => tool_calls.push(call.clone()),
                    }
                    if !send_delta(&sender, &cancellation, Ok(delta)).await {
                        persist_terminal(&store, conversation_id, &model, text, tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                        return Ok(())
                    }
                }
                Some(Err(error)) => {
                    let reason = error.to_string();
                    persist_terminal(
                        &store,
                        conversation_id,
                        &model,
                        text,
                        tool_calls,
                        AssistantMessageStatus::Failed,
                        Some(reason),
                    )
                    .await?;
                    let _ = sender.send(Err(error)).await;
                    return Ok(())
                }
                None => {
                    persist_terminal(&store, conversation_id, &model, text, tool_calls, AssistantMessageStatus::Complete, None).await?;
                    return Ok(())
                }
            }
        }
    }
}

async fn send_delta(
    sender: &mpsc::Sender<Result<ChatDelta>>,
    cancellation: &CancellationToken,
    item: Result<ChatDelta>,
) -> bool {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => false,
        _ = sender.closed() => false,
        result = sender.send(item) => result.is_ok(),
    }
}

async fn persist_terminal(
    store: &ConversationStore,
    conversation_id: &str,
    model: &str,
    text: String,
    tool_calls: Vec<crate::ToolCall>,
    status: AssistantMessageStatus,
    error: Option<String>,
) -> Result<()> {
    store
        .append_message(
            conversation_id,
            NewAssistantMessage::assistant(model.to_owned(), text, tool_calls, status, error),
        )
        .await
        .map(|_| ())
}
