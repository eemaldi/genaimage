use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use chrono::Utc;
use futures_util::{FutureExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot, watch};
#[cfg(test)]
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::store::NewAssistantToolSettlement;

use crate::{
    AssistantTool, ChatContent, ChatDelta, ChatMessage, ChatRequest, HarnessError, LlmProvider,
    LlmRoleBinding, Result, ToolCall, ToolContext, ToolDefinition,
};

use super::{
    AssistantConversation, AssistantConversationDetail, AssistantConversationListQuery,
    AssistantConversationPage, AssistantMessage, AssistantMessageStatus, AssistantToolConfirmation,
    AssistantToolConfirmationStatus, ConversationStore, NewAssistantConfirmation,
    NewAssistantConversation, NewAssistantMessage, NewAssistantToolAudit, PromptTemplates,
    SYSTEM_PROMPT_TEMPLATE,
};

const STREAM_CHANNEL_CAPACITY: usize = 32;
pub const DEFAULT_MAX_TOOL_STEPS: u32 = 8;

type GenerationResult = std::result::Result<AssistantMessage, String>;

#[derive(Clone)]
struct GenerationBinding {
    provider: Arc<dyn LlmProvider>,
    model: String,
    role: Option<LlmRoleBinding>,
}

#[derive(Clone)]
struct InFlight {
    id: Uuid,
    cancellation: CancellationToken,
    finished: watch::Receiver<Option<GenerationResult>>,
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
    user_message_id: String,
    finished: watch::Receiver<Option<GenerationResult>>,
}

impl AssistantStream {
    pub fn user_message_id(&self) -> &str {
        &self.user_message_id
    }

    pub async fn terminal_message(&mut self) -> Result<AssistantMessage> {
        loop {
            if let Some(result) = self.finished.borrow().clone() {
                return result.map_err(HarnessError::Database);
            }
            self.finished.changed().await.map_err(|error| {
                HarnessError::Database(format!(
                    "assistant generation completion channel closed: {error}"
                ))
            })?;
        }
    }
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

#[derive(Clone, Debug)]
pub enum AssistantConfirmationDecision {
    Confirm {
        preview_token: String,
    },
    Edit {
        arguments: Value,
        preview_token: String,
    },
    Reject(Option<String>),
}

#[derive(Clone, Debug)]
pub struct AssistantConfirmationPreview {
    pub preview: Option<Value>,
    pub preview_token: String,
}

#[derive(Clone)]
pub struct ConversationService {
    store: ConversationStore,
    templates: PromptTemplates,
    provider: Option<Arc<dyn LlmProvider>>,
    model: Option<String>,
    tools: Arc<BTreeMap<String, Arc<dyn AssistantTool>>>,
    max_tool_steps: u32,
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
            provider: Some(provider),
            model: Some(model),
            tools: Arc::new(BTreeMap::new()),
            max_tool_steps: DEFAULT_MAX_TOOL_STEPS,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            append_gate: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            append_started: Arc::new(Notify::new()),
        })
    }

    pub fn unbound(store: ConversationStore, templates: PromptTemplates) -> Self {
        Self {
            store,
            templates,
            provider: None,
            model: None,
            tools: Arc::new(BTreeMap::new()),
            max_tool_steps: DEFAULT_MAX_TOOL_STEPS,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            append_gate: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            append_started: Arc::new(Notify::new()),
        }
    }

    pub fn with_assistant_tools(mut self, tools: Vec<Arc<dyn AssistantTool>>) -> Result<Self> {
        let mut registered = BTreeMap::new();
        for tool in tools {
            let name = tool.name().to_owned();
            let schema = tool.json_schema();
            crate::tool_schema::validate_schema(&schema).map_err(|error| {
                HarnessError::Configuration(format!(
                    "assistant tool '{name}' has an unsupported JSON schema: {error}"
                ))
            })?;
            if name.trim().is_empty() {
                return Err(HarnessError::Configuration(
                    "assistant tool name must not be empty".into(),
                ));
            }
            if registered.insert(name.clone(), tool).is_some() {
                return Err(HarnessError::Configuration(format!(
                    "assistant tool '{name}' is already registered"
                )));
            }
        }
        self.tools = Arc::new(registered);
        Ok(self)
    }

    pub fn with_max_tool_steps(mut self, max_tool_steps: u32) -> Result<Self> {
        if max_tool_steps == 0 {
            return Err(HarnessError::Configuration(
                "maximum tool steps must be greater than zero".into(),
            ));
        }
        self.max_tool_steps = max_tool_steps;
        Ok(self)
    }

    pub async fn create_conversation(&self, title: String) -> Result<AssistantConversation> {
        let model = self.model.clone().ok_or_else(|| {
            HarnessError::AssistantNotConfigured("no text model is selected".into())
        })?;
        self.create_conversation_with_model(title, model).await
    }

    pub async fn create_conversation_with_model(
        &self,
        title: String,
        model: String,
    ) -> Result<AssistantConversation> {
        let template = self.templates.load(SYSTEM_PROMPT_TEMPLATE).await?;
        self.store
            .create(NewAssistantConversation {
                title,
                model,
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
        if !self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(conversation_id)
        {
            self.store
                .recover_invoking_confirmations(conversation_id)
                .await?;
        }
        self.store.get(conversation_id, message_limit).await
    }

    pub async fn append_message(
        &self,
        conversation_id: &str,
        message: NewAssistantMessage,
    ) -> Result<AssistantMessage> {
        self.store.append_message(conversation_id, message).await
    }

    pub async fn pending_confirmation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<AssistantToolConfirmation>> {
        self.store.pending_confirmation(conversation_id).await
    }

    pub async fn list_tool_audit(
        &self,
        conversation_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<super::AssistantToolAuditRecord>> {
        self.store.list_tool_audit(conversation_id, limit).await
    }

    pub async fn preview_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        arguments: Value,
    ) -> Result<AssistantConfirmationPreview> {
        let confirmation = self
            .store
            .get_confirmation(conversation_id, confirmation_id)
            .await?;
        if confirmation.status != AssistantToolConfirmationStatus::Pending {
            return Err(HarnessError::ToolConfirmationConflict(format!(
                "confirmation '{}' is not pending",
                confirmation.id
            )));
        }
        validate_arguments(&confirmation.json_schema, &arguments)
            .map_err(HarnessError::InvalidToolArguments)?;
        let tool = self.tools.get(&confirmation.tool).ok_or_else(|| {
            HarnessError::ToolConfirmationConflict(format!(
                "Assistant tool '{}' is no longer registered.",
                confirmation.tool
            ))
        })?;
        match confirmation_preview(Arc::clone(tool), &confirmation.id, &arguments).await {
            Ok(preview) => Ok(preview),
            Err(error) => Err(HarnessError::ToolConfirmationPreviewMismatch {
                detail: error.to_string(),
                preview: Some(json!({
                    "valid": false,
                    "issues": [{"field": "preview", "reason": error.to_string()}]
                })),
                preview_token: None,
                conflict: matches!(error, HarnessError::WorkflowChanged),
            }),
        }
    }

    pub async fn send_message(
        &self,
        conversation_id: &str,
        content: Vec<ChatContent>,
    ) -> Result<AssistantStream> {
        let provider = self.provider.clone().ok_or_else(|| {
            HarnessError::AssistantNotConfigured("no text provider is selected".into())
        })?;
        let model = self.model.clone().ok_or_else(|| {
            HarnessError::AssistantNotConfigured("no text model is selected".into())
        })?;
        self.send_message_with(conversation_id, content, provider, model, None)
            .await
    }

    pub async fn send_message_with_binding(
        &self,
        conversation_id: &str,
        content: Vec<ChatContent>,
        binding: LlmRoleBinding,
    ) -> Result<AssistantStream> {
        self.send_message_with(
            conversation_id,
            content,
            binding.provider(),
            binding.model().to_owned(),
            Some(binding),
        )
        .await
    }

    async fn send_message_with(
        &self,
        conversation_id: &str,
        content: Vec<ChatContent>,
        provider: Arc<dyn LlmProvider>,
        model: String,
        binding: Option<LlmRoleBinding>,
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
        let (finished_tx, finished_rx) = watch::channel(None);
        tokio::spawn(async move {
            supervise_generation(
                service,
                id,
                content,
                GenerationBinding {
                    provider,
                    model,
                    role: binding,
                },
                sender,
                started_tx,
                finished_tx,
            )
            .await;
        });
        let user_message_id = started_rx.await.map_err(|_| {
            HarnessError::ConversationConflict(
                "assistant generation supervisor stopped during setup".into(),
            )
        })??;
        Ok(AssistantStream {
            receiver,
            user_message_id,
            finished: finished_rx,
        })
    }

    pub async fn decide_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        decision: AssistantConfirmationDecision,
    ) -> Result<AssistantStream> {
        let provider = self.provider.clone().ok_or_else(|| {
            HarnessError::AssistantNotConfigured("no text provider is selected".into())
        })?;
        let model = self.model.clone().ok_or_else(|| {
            HarnessError::AssistantNotConfigured("no text model is selected".into())
        })?;
        self.decide_confirmation_inner(
            conversation_id,
            confirmation_id,
            decision,
            GenerationBinding {
                provider,
                model,
                role: None,
            },
        )
        .await
    }

    pub async fn decide_confirmation_with_binding(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        decision: AssistantConfirmationDecision,
        binding: LlmRoleBinding,
    ) -> Result<AssistantStream> {
        self.decide_confirmation_inner(
            conversation_id,
            confirmation_id,
            decision,
            GenerationBinding {
                provider: binding.provider(),
                model: binding.model().to_owned(),
                role: Some(binding),
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn decide_confirmation_inner(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
        decision: AssistantConfirmationDecision,
        binding: GenerationBinding,
    ) -> Result<AssistantStream> {
        let service = self.clone();
        let conversation_id = conversation_id.to_owned();
        let confirmation_id = confirmation_id.to_owned();
        let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let (started_tx, started_rx) = oneshot::channel();
        let (finished_tx, finished_rx) = watch::channel(None);
        tokio::spawn(async move {
            supervise_confirmation(
                service,
                conversation_id,
                confirmation_id,
                decision,
                binding,
                sender,
                started_tx,
                finished_tx,
            )
            .await;
        });
        let user_message_id = started_rx.await.map_err(|_| {
            HarnessError::ConversationConflict(
                "assistant confirmation supervisor stopped during setup".into(),
            )
        })??;
        Ok(AssistantStream {
            receiver,
            user_message_id,
            finished: finished_rx,
        })
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
                return result.map(|_| ()).map_err(HarnessError::Database);
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
    pub(crate) async fn test_claim_confirmation(
        &self,
        conversation_id: &str,
        confirmation_id: &str,
    ) -> Result<AssistantToolConfirmation> {
        let confirmation = self
            .store
            .get_confirmation(conversation_id, confirmation_id)
            .await?;
        self.store
            .claim_confirmation(
                conversation_id,
                confirmation_id,
                "confirm",
                Some(confirmation.proposed_arguments),
                None,
            )
            .await
    }

    #[cfg(test)]
    pub(crate) fn has_in_flight(&self, conversation_id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(conversation_id)
    }
}

fn acquire_lease(
    service: &ConversationService,
    conversation_id: &str,
    finished: watch::Receiver<Option<GenerationResult>>,
) -> Result<InFlightLease> {
    let cancellation = CancellationToken::new();
    let generation_id = Uuid::new_v4();
    let mut active = service
        .in_flight
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
            finished,
        },
    );
    Ok(InFlightLease {
        active: Arc::clone(&service.in_flight),
        conversation_id: conversation_id.to_owned(),
        generation_id,
        cancellation,
    })
}

#[allow(clippy::too_many_arguments)]
async fn supervise_generation(
    service: ConversationService,
    conversation_id: String,
    content: Vec<ChatContent>,
    binding: GenerationBinding,
    sender: mpsc::Sender<Result<ChatDelta>>,
    started: oneshot::Sender<Result<String>>,
    finished_tx: watch::Sender<Option<GenerationResult>>,
) {
    let mut started = Some(started);
    let lease = match acquire_lease(&service, &conversation_id, finished_tx.subscribe()) {
        Ok(lease) => lease,
        Err(error) => {
            fail_setup(&finished_tx, &mut started, error);
            return;
        }
    };
    let cancellation = lease.cancellation.clone();

    if let Err(error) = service
        .store
        .recover_invoking_confirmations(&conversation_id)
        .await
    {
        fail_setup(&finished_tx, &mut started, error);
        drop(lease);
        return;
    }
    let conversation = match service.store.get(&conversation_id, Some(500)).await {
        Ok(conversation) => conversation,
        Err(error) => {
            fail_setup(&finished_tx, &mut started, error);
            drop(lease);
            return;
        }
    };
    let pending = match service.store.pending_confirmation(&conversation_id).await {
        Ok(pending) => pending,
        Err(error) => {
            fail_setup(&finished_tx, &mut started, error);
            drop(lease);
            return;
        }
    };
    if pending.is_some() {
        fail_setup(
            &finished_tx,
            &mut started,
            HarnessError::ConversationConflict(
                "A tool confirmation is pending; decide it before sending a new message.".into(),
            ),
        );
        drop(lease);
        return;
    }

    let mut request = request_from_conversation(&service, conversation, &binding.model);
    request.messages.push(ChatMessage::User(content.clone()));
    if let Some(role) = &binding.role {
        if let Err(error) = role.check_request(&request) {
            fail_setup(&finished_tx, &mut started, error);
            drop(lease);
            return;
        }
    }
    if cancellation.is_cancelled() || sender.is_closed() {
        fail_setup(
            &finished_tx,
            &mut started,
            HarnessError::ConversationConflict("generation was cancelled during setup".into()),
        );
        drop(lease);
        return;
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

    let user_message = match service
        .store
        .append_message(&conversation_id, NewAssistantMessage::user(content))
        .await
    {
        Ok(message) => message,
        Err(error) => {
            fail_setup(&finished_tx, &mut started, error);
            drop(lease);
            return;
        }
    };
    if started
        .take()
        .is_some_and(|started| started.send(Ok(user_message.id.clone())).is_err())
    {
        cancellation.cancel();
    }
    let result = run_turn(
        &service,
        binding,
        &conversation_id,
        &user_message.id,
        request,
        0,
        cancellation.clone(),
        sender.clone(),
    )
    .await;
    let completion = result.as_ref().cloned().map_err(ToString::to_string);
    drop(lease);
    let _ = finished_tx.send(Some(completion));
}

#[allow(clippy::too_many_arguments)]
async fn supervise_confirmation(
    service: ConversationService,
    conversation_id: String,
    confirmation_id: String,
    decision: AssistantConfirmationDecision,
    binding: GenerationBinding,
    sender: mpsc::Sender<Result<ChatDelta>>,
    started: oneshot::Sender<Result<String>>,
    _finished_placeholder: watch::Sender<Option<GenerationResult>>,
) {
    let mut started = Some(started);
    let lease = match acquire_lease(
        &service,
        &conversation_id,
        _finished_placeholder.subscribe(),
    ) {
        Ok(value) => value,
        Err(error) => {
            fail_setup(&_finished_placeholder, &mut started, error);
            return;
        }
    };
    let cancellation = lease.cancellation.clone();
    if let Err(error) = service
        .store
        .recover_invoking_confirmations(&conversation_id)
        .await
    {
        fail_setup(&_finished_placeholder, &mut started, error);
        drop(lease);
        return;
    }
    let confirmation = match service
        .store
        .get_confirmation(&conversation_id, &confirmation_id)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            fail_setup(&_finished_placeholder, &mut started, error);
            drop(lease);
            return;
        }
    };
    if confirmation.status != AssistantToolConfirmationStatus::Pending {
        fail_setup(
            &_finished_placeholder,
            &mut started,
            HarnessError::ToolConfirmationConflict(format!(
                "confirmation '{}' was already decided",
                confirmation.id
            )),
        );
        drop(lease);
        return;
    }
    let detail = match service.store.get(&conversation_id, Some(500)).await {
        Ok(detail) => detail,
        Err(error) => {
            fail_setup(&_finished_placeholder, &mut started, error);
            drop(lease);
            return;
        }
    };
    let continuation_request = request_from_conversation(&service, detail, &binding.model);
    if let Some(role) = &binding.role {
        if let Err(error) = role.check_request(&continuation_request) {
            fail_setup(&_finished_placeholder, &mut started, error);
            drop(lease);
            return;
        }
    }

    let (decision_name, effective, reason) = match decision {
        AssistantConfirmationDecision::Confirm { preview_token } => {
            if let Err(error) = verify_confirmation_preview(
                &service,
                &confirmation,
                &confirmation.proposed_arguments,
                &preview_token,
            )
            .await
            {
                fail_setup(&_finished_placeholder, &mut started, error);
                drop(lease);
                return;
            }
            ("confirm", confirmation.proposed_arguments.clone(), None)
        }
        AssistantConfirmationDecision::Edit {
            arguments,
            preview_token,
        } => {
            if let Err(error) = validate_arguments(&confirmation.json_schema, &arguments) {
                let _ = service
                    .store
                    .append_tool_audit(NewAssistantToolAudit {
                        conversation_id: conversation_id.clone(),
                        confirmation_id: Some(confirmation.id.clone()),
                        tool_call_id: confirmation.tool_call_id.clone(),
                        tool: confirmation.tool.clone(),
                        proposed_arguments: confirmation.proposed_arguments.clone(),
                        validation_outcome: "valid".into(),
                        validation_error: Some(error.clone()),
                        decision: "edit_invalid".into(),
                        effective_arguments: Some(arguments),
                        decided_by: Some("user".into()),
                        invocation_ok: None,
                        invocation_result: None,
                        invocation_error: None,
                        decided_at: Some(Utc::now()),
                        invoked_at: None,
                    })
                    .await;
                fail_setup(
                    &_finished_placeholder,
                    &mut started,
                    HarnessError::InvalidToolArguments(error),
                );
                drop(lease);
                return;
            }
            if let Err(error) =
                verify_confirmation_preview(&service, &confirmation, &arguments, &preview_token)
                    .await
            {
                fail_setup(&_finished_placeholder, &mut started, error);
                drop(lease);
                return;
            }
            ("edit", arguments, None)
        }
        AssistantConfirmationDecision::Reject(reason) => ("reject", Value::Null, reason),
    };
    let claimed = match service
        .store
        .claim_confirmation(
            &conversation_id,
            &confirmation_id,
            decision_name,
            (decision_name != "reject").then(|| effective.clone()),
            reason,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            fail_setup(&_finished_placeholder, &mut started, error);
            drop(lease);
            return;
        }
    };
    let _ = started
        .take()
        .map(|started| started.send(Ok(confirmation.user_message_id.clone())));

    let result_event = if claimed.status == AssistantToolConfirmationStatus::Rejected {
        ToolOutcome::error(format!(
            "The user rejected tool '{}'.{}",
            confirmation.tool,
            claimed
                .decision_reason
                .as_deref()
                .map(|value| format!(" Reason: {value}"))
                .unwrap_or_default()
        ))
    } else if let Some(tool) = service.tools.get(&confirmation.tool) {
        let arguments = claimed.effective_arguments.clone().unwrap_or(effective);
        invoke_tool_uninterrupted(Arc::clone(tool), &confirmation, arguments).await
    } else {
        ToolOutcome::Err(format!(
            "Assistant tool '{}' is no longer registered.",
            confirmation.tool
        ))
    };

    let mut result = if claimed.status == AssistantToolConfirmationStatus::Rejected {
        let _ = send_delta(
            &sender,
            &cancellation,
            Ok(ChatDelta::ToolResult {
                tool_call_id: confirmation.tool_call_id.clone(),
                tool: confirmation.tool.clone(),
                ok: false,
                result: None,
                error: match &result_event {
                    ToolOutcome::Err(error) => Some(error.clone()),
                    ToolOutcome::Ok(_) => None,
                },
            }),
        )
        .await;
        service
            .store
            .get_message(&confirmation.source_message_id)
            .await
    } else {
        let settlement = match &result_event {
            ToolOutcome::Ok(value) => NewAssistantToolSettlement {
                ok: Some(true),
                result: Some(value.clone()),
                error: None,
            },
            ToolOutcome::Err(error) => NewAssistantToolSettlement {
                ok: Some(false),
                result: None,
                error: Some(error.clone()),
            },
        };
        let settled = service
            .store
            .settle_confirmation(&conversation_id, &confirmation_id, settlement)
            .await;
        let _ = send_delta(
            &sender,
            &cancellation,
            Ok(ChatDelta::ToolResult {
                tool_call_id: confirmation.tool_call_id.clone(),
                tool: confirmation.tool.clone(),
                ok: matches!(result_event, ToolOutcome::Ok(_)),
                result: match &result_event {
                    ToolOutcome::Ok(value) => Some(value.clone()),
                    ToolOutcome::Err(_) => None,
                },
                error: match &result_event {
                    ToolOutcome::Ok(_) => None,
                    ToolOutcome::Err(error) => Some(error.clone()),
                },
            }),
        )
        .await;
        match settled {
            Ok(_) => {
                service
                    .store
                    .get_message(&confirmation.source_message_id)
                    .await
            }
            Err(error) => Err(error),
        }
    };
    if let Ok(source) = result.as_ref() {
        result = continue_after_tool_calls(
            &service,
            binding,
            &conversation_id,
            &confirmation.user_message_id,
            source,
            &confirmation.tool_call_id,
            confirmation.model_step,
            cancellation.clone(),
            sender.clone(),
        )
        .await;
    }
    if let Err(error) = &result {
        let _ = send_delta(
            &sender,
            &cancellation,
            Err(HarnessError::Database(error.to_string())),
        )
        .await;
    }
    let completion = result.as_ref().cloned().map_err(ToString::to_string);
    drop(lease);
    let _ = _finished_placeholder.send(Some(completion));
}

fn fail_setup(
    finished: &watch::Sender<Option<GenerationResult>>,
    started: &mut Option<oneshot::Sender<Result<String>>>,
    error: HarnessError,
) {
    let _ = finished.send(Some(Err(error.to_string())));
    if let Some(started) = started.take() {
        let _ = started.send(Err(error));
    }
}

fn request_from_conversation(
    service: &ConversationService,
    conversation: AssistantConversationDetail,
    model: &str,
) -> ChatRequest {
    let mut messages = Vec::with_capacity(conversation.messages.len() + 1);
    messages.push(ChatMessage::System(conversation.conversation.system_prompt));
    messages.extend(
        conversation
            .messages
            .into_iter()
            .map(|message| message.content),
    );
    ChatRequest {
        model: model.to_owned(),
        messages,
        tools: tool_definitions(&service.tools),
    }
}

fn tool_definitions(tools: &BTreeMap<String, Arc<dyn AssistantTool>>) -> Vec<ToolDefinition> {
    tools
        .values()
        .map(|tool| ToolDefinition {
            name: tool.name().to_owned(),
            description: tool.description().to_owned(),
            input_schema: tool.json_schema(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    service: &ConversationService,
    binding: GenerationBinding,
    conversation_id: &str,
    user_message_id: &str,
    mut request: ChatRequest,
    mut completed_steps: u32,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
) -> Result<AssistantMessage> {
    loop {
        if completed_steps >= service.max_tool_steps {
            let message = format!(
                "Assistant tool loop exceeded the maximum of {} model steps for this turn.",
                service.max_tool_steps
            );
            return persist_terminal(
                &service.store,
                conversation_id,
                &binding.model,
                String::new(),
                Vec::new(),
                AssistantMessageStatus::Failed,
                Some(message),
            )
            .await;
        }
        if let Some(role) = &binding.role {
            role.check_request(&request)?;
        }
        completed_steps += 1;
        let output = stream_model_step(
            &service.store,
            binding.provider.clone(),
            &binding.model,
            conversation_id,
            request.clone(),
            cancellation.clone(),
            sender.clone(),
        )
        .await?;
        if output.tool_calls.is_empty() {
            return Ok(output.message);
        }
        request.messages.push(ChatMessage::Assistant {
            text: output.message.content_text(),
            tool_calls: output.tool_calls.clone(),
        });
        match process_tool_calls(
            service,
            conversation_id,
            user_message_id,
            &output.message,
            &output.tool_calls,
            0,
            completed_steps,
            cancellation.clone(),
            sender.clone(),
            &mut request,
        )
        .await?
        {
            ToolCallsOutcome::Complete => {}
            ToolCallsOutcome::Awaiting(message) | ToolCallsOutcome::Cancelled(message) => {
                return Ok(message)
            }
        }
    }
}

struct ModelOutput {
    message: AssistantMessage,
    tool_calls: Vec<ToolCall>,
}

async fn stream_model_step(
    store: &ConversationStore,
    provider: Arc<dyn LlmProvider>,
    model: &str,
    conversation_id: &str,
    request: ChatRequest,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
) -> Result<ModelOutput> {
    let provider_stream = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            let message = persist_terminal(store, conversation_id, model, String::new(), Vec::new(), AssistantMessageStatus::Cancelled, None).await?;
            return Ok(ModelOutput { message, tool_calls: Vec::new() });
        }
        _ = sender.closed() => {
            let message = persist_terminal(store, conversation_id, model, String::new(), Vec::new(), AssistantMessageStatus::Cancelled, None).await?;
            return Ok(ModelOutput { message, tool_calls: Vec::new() });
        }
        result = std::panic::AssertUnwindSafe(provider.chat_stream(request)).catch_unwind() => match result {
            Ok(result) => result,
            Err(_) => Err(HarnessError::Configuration("provider startup panicked".into())),
        },
    };
    let mut stream = match provider_stream {
        Ok(stream) => stream,
        Err(error) => {
            let reason = error.to_string();
            let message = persist_terminal(
                store,
                conversation_id,
                model,
                String::new(),
                Vec::new(),
                AssistantMessageStatus::Failed,
                Some(reason),
            )
            .await?;
            let _ = send_delta(&sender, &cancellation, Err(error)).await;
            return Ok(ModelOutput {
                message,
                tool_calls: Vec::new(),
            });
        }
    };
    let mut output = AssistantOutput::default();
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                drop(stream);
                let terminal = persist_terminal(store, conversation_id, model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(ModelOutput { message: terminal, tool_calls: Vec::new() });
            }
            _ = sender.closed() => {
                drop(stream);
                let terminal = persist_terminal(store, conversation_id, model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                return Ok(ModelOutput { message: terminal, tool_calls: Vec::new() });
            }
            item = std::panic::AssertUnwindSafe(stream.next()).catch_unwind() => match item {
                Ok(Some(Ok(delta @ ChatDelta::Text(_)))) | Ok(Some(Ok(delta @ ChatDelta::ToolCall(_)))) => {
                    output.record(&delta);
                    if !send_delta(&sender, &cancellation, Ok(delta)).await {
                        drop(stream);
                        let terminal = persist_terminal(store, conversation_id, model, output.text, output.tool_calls, AssistantMessageStatus::Cancelled, None).await?;
                        return Ok(ModelOutput { message: terminal, tool_calls: Vec::new() });
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => {
                    let reason = error.to_string();
                    drop(stream);
                    let message = persist_terminal(store, conversation_id, model, output.text, output.tool_calls, AssistantMessageStatus::Failed, Some(reason)).await?;
                    let _ = send_delta(&sender, &cancellation, Err(error)).await;
                    return Ok(ModelOutput { message, tool_calls: Vec::new() });
                }
                Ok(None) => {
                    let calls = output.tool_calls.clone();
                    let terminal = persist_terminal(store, conversation_id, model, output.text, calls.clone(), AssistantMessageStatus::Complete, None).await?;
                    return Ok(ModelOutput { message: terminal, tool_calls: calls });
                }
                Err(_) => {
                    let reason = "provider stream panicked".to_owned();
                    drop(stream);
                    let message = persist_terminal(store, conversation_id, model, output.text, output.tool_calls, AssistantMessageStatus::Failed, Some(reason.clone())).await?;
                    let _ = send_delta(&sender, &cancellation, Err(HarnessError::Configuration(reason))).await;
                    return Ok(ModelOutput { message, tool_calls: Vec::new() });
                }
            }
        }
    }
}

enum ToolCallsOutcome {
    Complete,
    Awaiting(AssistantMessage),
    Cancelled(AssistantMessage),
}

enum ToolOutcome {
    Ok(Value),
    Err(String),
}
impl ToolOutcome {
    fn error(error: String) -> Self {
        Self::Err(error)
    }
    fn value(&self) -> Value {
        match self {
            Self::Ok(value) => json!({"ok": true, "result": value}),
            Self::Err(error) => json!({"ok": false, "error": error}),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_tool_calls(
    service: &ConversationService,
    conversation_id: &str,
    user_message_id: &str,
    source: &AssistantMessage,
    calls: &[ToolCall],
    start: usize,
    model_step: u32,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
    request: &mut ChatRequest,
) -> Result<ToolCallsOutcome> {
    for call in &calls[start..] {
        let Some(tool) = service.tools.get(&call.name) else {
            let error = format!("Unknown assistant tool '{}'.", call.name);
            service
                .store
                .append_tool_audit(unknown_audit(conversation_id, call, error.clone()))
                .await?;
            append_tool_outcome(
                service,
                &sender,
                &cancellation,
                conversation_id,
                &call.id,
                &call.name,
                &ToolOutcome::Err(error),
            )
            .await?;
            request.messages.push(tool_message(
                call,
                &ToolOutcome::Err(format!("Unknown assistant tool '{}'.", call.name)),
            ));
            continue;
        };
        if let Err(error) = validate_arguments(&tool.json_schema(), &call.arguments) {
            service
                .store
                .append_tool_audit(invalid_audit(conversation_id, call, error.clone()))
                .await?;
            append_tool_outcome(
                service,
                &sender,
                &cancellation,
                conversation_id,
                &call.id,
                &call.name,
                &ToolOutcome::Err(error.clone()),
            )
            .await?;
            request
                .messages
                .push(tool_message(call, &ToolOutcome::Err(error)));
            continue;
        }
        if tool.requires_confirmation() {
            let preview = match tool.preview(&call.arguments).await {
                Ok(preview) => preview,
                Err(error) => {
                    let error = error.to_string();
                    service
                        .store
                        .append_tool_audit(invalid_audit(conversation_id, call, error.clone()))
                        .await?;
                    append_tool_outcome(
                        service,
                        &sender,
                        &cancellation,
                        conversation_id,
                        &call.id,
                        &call.name,
                        &ToolOutcome::Err(error.clone()),
                    )
                    .await?;
                    request
                        .messages
                        .push(tool_message(call, &ToolOutcome::Err(error)));
                    continue;
                }
            };
            let confirmation = service
                .store
                .create_confirmation(NewAssistantConfirmation {
                    conversation_id: conversation_id.to_owned(),
                    tool_call_id: call.id.clone(),
                    tool: call.name.clone(),
                    proposed_arguments: call.arguments.clone(),
                    json_schema: tool.json_schema(),
                    source_message_id: source.id.clone(),
                    user_message_id: user_message_id.to_owned(),
                    model_step,
                })
                .await?;
            let terminal = service.store.get_message(&source.id).await?;
            let _ = send_delta(
                &sender,
                &cancellation,
                Ok(ChatDelta::ConfirmationRequired {
                    confirmation_id: confirmation.id.clone(),
                    conversation_id: conversation_id.to_owned(),
                    tool_call_id: call.id.clone(),
                    tool: call.name.clone(),
                    arguments: call.arguments.clone(),
                    json_schema: tool.json_schema(),
                    preview_token: preview_token(&confirmation.id, &call.arguments, &preview),
                    preview,
                }),
            )
            .await;
            return Ok(ToolCallsOutcome::Awaiting(terminal));
        }
        let outcome = invoke_tool(
            service,
            Arc::clone(tool),
            &confirmation_context(conversation_id, call, None),
            call.arguments.clone(),
            cancellation.clone(),
            sender.clone(),
        )
        .await;
        if matches!(outcome, ToolOutcome::Err(ref error) if error == "Tool invocation cancelled.") {
            service
                .store
                .append_tool_audit(invocation_audit(
                    conversation_id,
                    call,
                    &outcome,
                    None,
                    "automatic",
                ))
                .await?;
            let terminal = service
                .store
                .set_message_status(
                    conversation_id,
                    &source.id,
                    AssistantMessageStatus::Cancelled,
                    None,
                )
                .await?;
            return Ok(ToolCallsOutcome::Cancelled(terminal));
        }
        service
            .store
            .append_tool_audit(invocation_audit(
                conversation_id,
                call,
                &outcome,
                None,
                "automatic",
            ))
            .await?;
        append_tool_outcome(
            service,
            &sender,
            &cancellation,
            conversation_id,
            &call.id,
            &call.name,
            &outcome,
        )
        .await?;
        request.messages.push(tool_message(call, &outcome));
    }
    Ok(ToolCallsOutcome::Complete)
}

#[allow(clippy::too_many_arguments)]
async fn continue_after_tool_calls(
    service: &ConversationService,
    binding: GenerationBinding,
    conversation_id: &str,
    user_message_id: &str,
    source: &AssistantMessage,
    resolved_call_id: &str,
    completed_steps: u32,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
) -> Result<AssistantMessage> {
    let ChatMessage::Assistant { tool_calls, .. } = &source.content else {
        return Err(HarnessError::Database(
            "confirmation source message is not an assistant tool proposal".into(),
        ));
    };
    let index = tool_calls
        .iter()
        .position(|call| call.id == resolved_call_id)
        .ok_or_else(|| {
            HarnessError::Database("confirmation call is absent from the source proposal".into())
        })?;
    let detail = service.store.get(conversation_id, Some(500)).await?;
    let mut request = request_from_conversation(service, detail, &binding.model);
    if let Some(next) = tool_calls.get(index + 1) {
        if let ToolCallsOutcome::Awaiting(message) | ToolCallsOutcome::Cancelled(message) =
            process_tool_calls(
                service,
                conversation_id,
                user_message_id,
                source,
                tool_calls,
                index + 1,
                completed_steps,
                cancellation.clone(),
                sender.clone(),
                &mut request,
            )
            .await?
        {
            return Ok(message);
        }
        let _ = next;
    }
    run_turn(
        service,
        binding,
        conversation_id,
        user_message_id,
        request,
        completed_steps,
        cancellation,
        sender,
    )
    .await
}

fn confirmation_context(
    conversation_id: &str,
    call: &ToolCall,
    confirmation_id: Option<String>,
) -> AssistantToolConfirmation {
    let now = Utc::now();
    AssistantToolConfirmation {
        id: confirmation_id.unwrap_or_default(),
        conversation_id: conversation_id.to_owned(),
        tool_call_id: call.id.clone(),
        tool: call.name.clone(),
        proposed_arguments: call.arguments.clone(),
        json_schema: Value::Null,
        source_message_id: String::new(),
        user_message_id: String::new(),
        model_step: 0,
        status: AssistantToolConfirmationStatus::Pending,
        decision: None,
        effective_arguments: None,
        decision_reason: None,
        created_at: now,
        decided_at: None,
    }
}

async fn confirmation_preview(
    tool: Arc<dyn AssistantTool>,
    confirmation_id: &str,
    arguments: &Value,
) -> Result<AssistantConfirmationPreview> {
    let preview = tool.preview(arguments).await?;
    Ok(AssistantConfirmationPreview {
        preview_token: preview_token(confirmation_id, arguments, &preview),
        preview,
    })
}

fn preview_token(confirmation_id: &str, arguments: &Value, preview: &Option<Value>) -> String {
    let canonical = json!({
        "arguments": arguments,
        "confirmation_id": confirmation_id,
        "preview": preview
    });
    hex::encode(Sha256::digest(
        serde_json::to_vec(&canonical).expect("JSON values are serialisable"),
    ))
}

async fn verify_confirmation_preview(
    service: &ConversationService,
    confirmation: &AssistantToolConfirmation,
    arguments: &Value,
    supplied_token: &str,
) -> Result<AssistantConfirmationPreview> {
    let tool = service.tools.get(&confirmation.tool).ok_or_else(|| {
        HarnessError::ToolConfirmationConflict(format!(
            "Assistant tool '{}' is no longer registered.",
            confirmation.tool
        ))
    })?;
    match confirmation_preview(Arc::clone(tool), &confirmation.id, arguments).await {
        Ok(current) if current.preview_token == supplied_token => Ok(current),
        Ok(current) => Err(HarnessError::ToolConfirmationPreviewMismatch {
            detail: "The confirmation preview does not match these arguments; preview them again before deciding.".into(),
            preview: current.preview,
            preview_token: Some(current.preview_token),
            conflict: false,
        }),
        Err(error) => Err(HarnessError::ToolConfirmationPreviewMismatch {
            detail: error.to_string(),
            preview: Some(json!({
                "valid": false,
                "issues": [{"field": "preview", "reason": error.to_string()}]
            })),
            preview_token: None,
            conflict: matches!(error, HarnessError::WorkflowChanged),
        }),
    }
}

async fn invoke_tool_uninterrupted(
    tool: Arc<dyn AssistantTool>,
    confirmation: &AssistantToolConfirmation,
    arguments: Value,
) -> ToolOutcome {
    let context = ToolContext {
        conversation_id: confirmation.conversation_id.clone(),
        tool_call_id: confirmation.tool_call_id.clone(),
        confirmation_id: Some(confirmation.id.clone()),
    };
    match std::panic::AssertUnwindSafe(tool.invoke(context, arguments))
        .catch_unwind()
        .await
    {
        Ok(Ok(value)) => ToolOutcome::Ok(value),
        Ok(Err(error)) => ToolOutcome::Err(error.to_string()),
        Err(_) => ToolOutcome::Err("Tool invocation panicked.".into()),
    }
}

async fn invoke_tool(
    _service: &ConversationService,
    tool: Arc<dyn AssistantTool>,
    confirmation: &AssistantToolConfirmation,
    arguments: Value,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ChatDelta>>,
) -> ToolOutcome {
    let context = ToolContext {
        conversation_id: confirmation.conversation_id.clone(),
        tool_call_id: confirmation.tool_call_id.clone(),
        confirmation_id: (!confirmation.id.is_empty()).then(|| confirmation.id.clone()),
    };
    tokio::select! {
        biased;
        () = cancellation.cancelled() => ToolOutcome::Err("Tool invocation cancelled.".into()),
        _ = sender.closed() => ToolOutcome::Err("Tool invocation cancelled.".into()),
        result = std::panic::AssertUnwindSafe(tool.invoke(context, arguments)).catch_unwind() => match result {
            Ok(Ok(value)) => ToolOutcome::Ok(value),
            Ok(Err(error)) => ToolOutcome::Err(error.to_string()),
            Err(_) => ToolOutcome::Err("Tool invocation panicked.".into()),
        },
    }
}

fn validate_arguments(schema: &Value, arguments: &Value) -> std::result::Result<(), String> {
    crate::tool_schema::validate_instance(schema, arguments)
}

async fn append_tool_outcome(
    service: &ConversationService,
    sender: &mpsc::Sender<Result<ChatDelta>>,
    cancellation: &CancellationToken,
    conversation_id: &str,
    call_id: &str,
    tool: &str,
    outcome: &ToolOutcome,
) -> Result<()> {
    service
        .store
        .append_message(
            conversation_id,
            NewAssistantMessage::tool(call_id.to_owned(), outcome.value()),
        )
        .await?;
    let _ = send_delta(
        sender,
        cancellation,
        Ok(ChatDelta::ToolResult {
            tool_call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            ok: matches!(outcome, ToolOutcome::Ok(_)),
            result: match outcome {
                ToolOutcome::Ok(value) => Some(value.clone()),
                ToolOutcome::Err(_) => None,
            },
            error: match outcome {
                ToolOutcome::Ok(_) => None,
                ToolOutcome::Err(error) => Some(error.clone()),
            },
        }),
    )
    .await;
    Ok(())
}

fn tool_message(call: &ToolCall, outcome: &ToolOutcome) -> ChatMessage {
    ChatMessage::Tool {
        call_id: call.id.clone(),
        result: outcome.value(),
    }
}

fn unknown_audit(conversation_id: &str, call: &ToolCall, error: String) -> NewAssistantToolAudit {
    NewAssistantToolAudit {
        conversation_id: conversation_id.to_owned(),
        confirmation_id: None,
        tool_call_id: call.id.clone(),
        tool: call.name.clone(),
        proposed_arguments: call.arguments.clone(),
        validation_outcome: "unknown_tool".into(),
        validation_error: Some(error.clone()),
        decision: "not_invoked".into(),
        effective_arguments: None,
        decided_by: None,
        invocation_ok: Some(false),
        invocation_result: None,
        invocation_error: Some(error),
        decided_at: None,
        invoked_at: None,
    }
}

fn invalid_audit(conversation_id: &str, call: &ToolCall, error: String) -> NewAssistantToolAudit {
    NewAssistantToolAudit {
        conversation_id: conversation_id.to_owned(),
        confirmation_id: None,
        tool_call_id: call.id.clone(),
        tool: call.name.clone(),
        proposed_arguments: call.arguments.clone(),
        validation_outcome: "invalid".into(),
        validation_error: Some(error.clone()),
        decision: "not_invoked".into(),
        effective_arguments: None,
        decided_by: None,
        invocation_ok: Some(false),
        invocation_result: None,
        invocation_error: Some(error),
        decided_at: None,
        invoked_at: None,
    }
}

fn invocation_audit(
    conversation_id: &str,
    call: &ToolCall,
    outcome: &ToolOutcome,
    confirmation_id: Option<String>,
    decision: &str,
) -> NewAssistantToolAudit {
    NewAssistantToolAudit {
        conversation_id: conversation_id.to_owned(),
        confirmation_id,
        tool_call_id: call.id.clone(),
        tool: call.name.clone(),
        proposed_arguments: call.arguments.clone(),
        validation_outcome: "valid".into(),
        validation_error: None,
        decision: decision.to_owned(),
        effective_arguments: Some(call.arguments.clone()),
        decided_by: None,
        invocation_ok: Some(matches!(outcome, ToolOutcome::Ok(_))),
        invocation_result: match outcome {
            ToolOutcome::Ok(value) => Some(value.clone()),
            ToolOutcome::Err(_) => None,
        },
        invocation_error: match outcome {
            ToolOutcome::Ok(_) => None,
            ToolOutcome::Err(error) => Some(error.clone()),
        },
        decided_at: None,
        invoked_at: Some(Utc::now()),
    }
}

#[derive(Default)]
struct AssistantOutput {
    text: String,
    tool_calls: Vec<ToolCall>,
}
impl AssistantOutput {
    fn record(&mut self, delta: &ChatDelta) {
        match delta {
            ChatDelta::Text(value) => self.text.push_str(value),
            ChatDelta::ToolCall(call) => self.tool_calls.push(call.clone()),
            _ => {}
        }
    }
}

impl AssistantMessage {
    fn content_text(&self) -> String {
        match &self.content {
            ChatMessage::Assistant { text, .. } => text.clone(),
            _ => String::new(),
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
    tool_calls: Vec<ToolCall>,
    status: AssistantMessageStatus,
    error: Option<String>,
) -> Result<AssistantMessage> {
    store
        .append_message(
            conversation_id,
            NewAssistantMessage::assistant(model.to_owned(), text, tool_calls, status, error),
        )
        .await
}
