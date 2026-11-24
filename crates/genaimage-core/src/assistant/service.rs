use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use futures_util::{FutureExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
#[cfg(test)]
use tokio::sync::{Notify, Semaphore};
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

struct InFlightLease {
    active: Arc<Mutex<HashMap<String, InFlight>>>,
    conversation_id: String,
    generation_id: Uuid,
    cancellation: CancellationToken,
}

impl Drop for InFlightLease {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active
            .get(&self.conversation_id)
            .is_some_and(|generation| generation.id == self.generation_id)
        {
            active.remove(&self.conversation_id);
        }
    }
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
    #[cfg(test)]
    append_gate: Arc<Semaphore>,
    #[cfg(test)]
    append_started: Arc<Notify>,
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
            #[cfg(test)]
            append_gate: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            append_started: Arc::new(Notify::new()),
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

        let service = self.clone();
        let id = conversation_id.to_owned();
        let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let (started_tx, started_rx) = oneshot::channel();
        tokio::spawn(async move {
            supervise_generation(service, id, content, sender, started_tx).await;
        });
        started_rx.await.map_err(|_| {
            HarnessError::ConversationConflict(
                "assistant generation supervisor stopped during setup".into(),
            )
        })??;
        Ok(AssistantStream { receiver })
    }

    pub async fn cancel(&self, conversation_id: &str) -> Result<()> {
        let active = {
            let in_flight = self
                .in_flight
                .lock()
                .unwrap_or_else(|error| error.into_inner());
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
            let active = self
                .in_flight
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains_key(conversation_id);
            if !active {
                return self.store.delete(conversation_id).await;
            }
            self.cancel(conversation_id).await?;
        }
    }

    #[cfg(test)]
    pub(crate) fn test_append_controls(&self) -> (Arc<Notify>, Arc<Semaphore>) {
        (
            Arc::clone(&self.append_started),
            Arc::clone(&self.append_gate),
        )
    }

    #[cfg(test)]
    pub(crate) fn has_in_flight(&self, conversation_id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(conversation_id)
    }
}

async fn supervise_generation(
    service: ConversationService,
    conversation_id: String,
    content: Vec<ChatContent>,
    sender: mpsc::Sender<Result<ChatDelta>>,
    started: oneshot::Sender<Result<()>>,
) {
    let mut started = Some(started);
    let result = async {
        let cancellation = CancellationToken::new();
        let generation_id = Uuid::new_v4();
        let (finished_tx, finished_rx) = watch::channel(None);
        let lease = {
            let mut active = service
                .in_flight
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if active.contains_key(&conversation_id) {
                return Err(HarnessError::ConversationConflict(format!(
                    "conversation '{conversation_id}' already has a generation in flight"
                )));
            }
            active.insert(
                conversation_id.clone(),
                InFlight {
                    id: generation_id,
                    cancellation: cancellation.clone(),
                    finished: finished_rx,
                },
            );
            InFlightLease {
                active: Arc::clone(&service.in_flight),
                conversation_id: conversation_id.clone(),
                generation_id,
                cancellation: cancellation.clone(),
            }
        };

        let conversation = match service.store.get(&conversation_id, Some(1)).await {
            Ok(conversation) => conversation,
            Err(error) => {
                let message = error.to_string();
                let _ = finished_tx.send(Some(Err(message)));
                drop(lease);
                if let Some(started) = started.take() {
                    let _ = started.send(Err(error));
                }
                return Ok(());
            }
        };
        if conversation.conversation.model != service.model {
            let error = HarnessError::ConversationConflict(format!(
                "conversation '{}' is pinned to model '{}', not configured model '{}'",
                conversation_id, conversation.conversation.model, service.model
            ));
            let message = error.to_string();
            let _ = finished_tx.send(Some(Err(message)));
            drop(lease);
            if let Some(started) = started.take() {
                let _ = started.send(Err(error));
            }
            return Ok(());
        }
        if cancellation.is_cancelled() || sender.is_closed() {
            let _ = finished_tx.send(Some(Ok(())));
            drop(lease);
            if let Some(started) = started.take() {
                let _ = started.send(Ok(()));
            }
            return Ok(());
        }

        #[cfg(test)]
        {
            service.append_started.notify_one();
            let _permit = service
                .append_gate
                .acquire()
                .await
                .expect("append gate is never closed");
        }

        let append = service
            .store
            .append_message(&conversation_id, NewAssistantMessage::user(content))
            .await;
        if let Err(error) = append {
            let message = error.to_string();
            let _ = finished_tx.send(Some(Err(message)));
            drop(lease);
            if let Some(started) = started.take() {
                let _ = started.send(Err(error));
            }
            return Ok(());
        }

        let detail = match service.store.get(&conversation_id, Some(500)).await {
            Ok(detail) => detail,
            Err(error) => {
                persist_terminal(
                    &service.store,
                    &conversation_id,
                    &service.model,
                    String::new(),
                    Vec::new(),
                    AssistantMessageStatus::Cancelled,
                    None,
                )
                .await?;
                let message = error.to_string();
                let _ = finished_tx.send(Some(Err(message)));
                drop(lease);
                if let Some(started) = started.take() {
                    let _ = started.send(Err(error));
                }
                return Ok(());
            }
        };
        let mut messages = Vec::with_capacity(detail.messages.len() + 1);
        messages.push(ChatMessage::System(detail.conversation.system_prompt));
        messages.extend(detail.messages.into_iter().map(|message| message.content));
        let request = ChatRequest {
            model: service.model.clone(),
            messages,
            tools: Vec::new(),
        };
        if started
            .take()
            .is_some_and(|started| started.send(Ok(())).is_err())
        {
            cancellation.cancel();
        }

        let result = run_generation(
            service.store.clone(),
            Arc::clone(&service.provider),
            service.model.clone(),
            &conversation_id,
            request,
            cancellation,
            sender,
        )
        .await;
        let completion = result.as_ref().map_err(ToString::to_string).map(|_| ());
        drop(lease);
        let _ = finished_tx.send(Some(completion));
        result
    }
    .await;

    if let (Err(error), Some(started)) = (result, started) {
        let _ = started.send(Err(error));
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
    let mut output = AssistantOutput::default();
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                drop(stream);
                persist_terminal(&store, conversation_id, &model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(())
            }
            _ = sender.closed() => {
                drop(stream);
                persist_terminal(&store, conversation_id, &model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(())
            }
            item = std::panic::AssertUnwindSafe(stream.next()).catch_unwind() => match item {
                Ok(Some(Ok(delta))) => {
                    output.record(&delta);
                    if !send_delta(&sender, &cancellation, Ok(delta)).await {
                        drop(stream);
                        persist_terminal(&store, conversation_id, &model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                        return Ok(())
                    }
                }
                Ok(Some(Err(error))) => {
                    let reason = error.to_string();
                    drop(stream);
                    persist_terminal(
                        &store,
                        conversation_id,
                        &model,
                        output.text,
                        output.tool_calls,
                        AssistantMessageStatus::Failed,
                        Some(reason),
                    )
                    .await?;
                    let _ = send_delta(&sender, &cancellation, Err(error)).await;
                    return Ok(())
                }
                Ok(None) => {
                    persist_terminal(&store, conversation_id, &model, output.text, output.tool_calls, AssistantMessageStatus::Complete, None).await?;
                    return Ok(())
                }
                Err(_) => {
                    drop(stream);
                    let reason = "provider stream panicked".to_owned();
                    persist_terminal(
                        &store,
                        conversation_id,
                        &model,
                        output.text,
                        output.tool_calls,
                        AssistantMessageStatus::Failed,
                        Some(reason.clone()),
                    )
                    .await?;
                    let _ = send_delta(
                        &sender,
                        &cancellation,
                        Err(HarnessError::Configuration(reason)),
                    )
                    .await;
                    return Ok(())
                }
            }
        }
    }
}

#[derive(Default)]
struct AssistantOutput {
    text: String,
    tool_calls: Vec<crate::ToolCall>,
}

impl AssistantOutput {
    fn record(&mut self, delta: &ChatDelta) {
        match delta {
            ChatDelta::Text(value) => self.text.push_str(value),
            ChatDelta::ToolCall(call) => self.tool_calls.push(call.clone()),
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
