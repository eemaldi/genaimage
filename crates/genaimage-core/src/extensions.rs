//! Extension contracts and explicit application wiring. No runtime plugin loading.

use std::{collections::BTreeMap, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::ComfyUiProfile, EventBus, EventEnvelope, HarnessError, ImageBackend, ReceiveError,
    ResolvedConfig, Result, ResyncReason,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMessage {
    System(String),
    User(Vec<ChatContent>),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        result: Value,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatContent {
    Text(String),
    Image { media_type: String, bytes: Vec<u8> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolContext {
    pub conversation_id: String,
    pub tool_call_id: String,
    /// Stable confirmation identifier. Confirmed tools can use this as an
    /// idempotency key when resuming an interrupted invocation.
    pub confirmation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatDelta {
    Text(String),
    /// A complete call with parsed arguments, not an incremental JSON fragment.
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        tool: String,
        ok: bool,
        result: Option<Value>,
        error: Option<String>,
    },
    ConfirmationRequired {
        confirmation_id: String,
        conversation_id: String,
        tool_call_id: String,
        tool: String,
        arguments: Value,
        json_schema: Value,
        preview: Option<Value>,
        preview_token: String,
    },
}

/// Dropping the stream must cancel in-flight provider work and release its resources.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatDelta>> + Send>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmModel {
    pub id: String,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

/// Streams chat for the explicitly selected model, including tool proposals and
/// vision inputs. Unsupported capabilities must fail rather than silently
/// switch models or discard prompts, images or tools. Reject a request for
/// known unsupported capabilities with [`HarnessError::Configuration`] naming
/// the model and capability before returning a stream.
///
/// Tool calls are proposals, not confirmation or permission to submit a run.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream>;
    async fn list_models(&self) -> Result<Vec<LlmModel>>;
}

/// The tool runtime validates arguments against the schema and obtains any
/// required user confirmation before invoking the tool. `requires_confirmation`
/// is metadata, not enforcement. An assistant must never submit a run without
/// explicit user approval of the effective proposal, regardless of tool metadata.
/// A confirmation tool may implement the read-only `preview` hook so the pause
/// payload can show values resolved from presets, defaults and inherited inputs.
/// The runtime supplies the invocation context after approval; tools must treat
/// `confirmation_id` as an idempotency key for confirmed operations.
#[async_trait]
pub trait AssistantTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn json_schema(&self) -> Value;
    fn requires_confirmation(&self) -> bool;
    async fn preview(&self, _arguments: &Value) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn invoke(&self, context: ToolContext, arguments: Value) -> Result<Value>;
}

/// A sink must resynchronise its state when delivery reports a gap.
#[derive(Clone, Debug)]
pub enum SinkNotification {
    Event(Box<EventEnvelope>),
    ResyncRequired(ResyncReason),
}

/// Consumes bus notifications away from the publisher's execution path.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn handle(&self, notification: &SinkNotification) -> Result<()>;
}

/// Holds application-supplied extensions without depending on concrete adapters.
/// The current configuration selects a single image backend; other extension
/// kinds are available for explicit registration, not configured yet.
#[derive(Default)]
pub struct ExtensionRegistry {
    image_backends: BTreeMap<String, Arc<dyn ImageBackend>>,
    llm_providers: BTreeMap<String, Arc<dyn LlmProvider>>,
    assistant_tools: BTreeMap<String, Arc<dyn AssistantTool>>,
    event_sinks: BTreeMap<String, Arc<dyn EventSink>>,
}

pub const COMFYUI_BACKEND: &str = "comfyui";

impl ExtensionRegistry {
    /// Build the configured ComfyUI backend with the supplied adapter factory.
    /// A factory failure is returned, never replaced by a default.
    pub fn from_config<B: ImageBackend + 'static>(
        config: &ResolvedConfig,
        connect: impl FnOnce(&ComfyUiProfile) -> Result<B>,
    ) -> Result<Self> {
        let mut registry = Self::default();
        registry.register_image_backend(COMFYUI_BACKEND, Arc::new(connect(&config.comfyui)?))?;
        Ok(registry)
    }

    pub fn register_image_backend(
        &mut self,
        name: &str,
        backend: Arc<dyn ImageBackend>,
    ) -> Result<()> {
        register(&mut self.image_backends, "image backend", name, backend)
    }

    pub fn image_backend(&self, name: &str) -> Option<Arc<dyn ImageBackend>> {
        self.image_backends.get(name).cloned()
    }

    pub fn register_llm_provider(
        &mut self,
        name: &str,
        provider: Arc<dyn LlmProvider>,
    ) -> Result<()> {
        register(&mut self.llm_providers, "LLM provider", name, provider)
    }

    pub fn llm_provider(&self, name: &str) -> Option<Arc<dyn LlmProvider>> {
        self.llm_providers.get(name).cloned()
    }

    pub fn register_assistant_tool(&mut self, tool: Arc<dyn AssistantTool>) -> Result<()> {
        let name = tool.name().to_owned();
        let schema = tool.json_schema();
        crate::tool_schema::validate_schema(&schema).map_err(|error| {
            HarnessError::Configuration(format!(
                "assistant tool '{name}' has an unsupported JSON schema: {error}"
            ))
        })?;
        register(&mut self.assistant_tools, "assistant tool", &name, tool)
    }

    pub fn assistant_tool(&self, name: &str) -> Option<Arc<dyn AssistantTool>> {
        self.assistant_tools.get(name).cloned()
    }

    pub fn assistant_tools(&self) -> Vec<Arc<dyn AssistantTool>> {
        self.assistant_tools.values().cloned().collect()
    }

    pub fn register_event_sink(&mut self, name: &str, sink: Arc<dyn EventSink>) -> Result<()> {
        register(&mut self.event_sinks, "event sink", name, sink)
    }

    pub fn event_sink(&self, name: &str) -> Option<Arc<dyn EventSink>> {
        self.event_sinks.get(name).cloned()
    }

    /// Start one independent forwarding task per registered sink. Retain the
    /// handles to observe sink errors; a lagging sink is notified and stops.
    /// Call once per bus after registration, from a Tokio runtime.
    pub fn forward_event_sinks(
        &self,
        bus: &EventBus,
    ) -> BTreeMap<String, tokio::task::JoinHandle<Result<()>>> {
        self.event_sinks
            .iter()
            .map(|(name, sink)| {
                let mut subscription = bus.subscribe();
                let sink = Arc::clone(sink);
                let task = tokio::spawn(async move {
                    loop {
                        match subscription.recv().await {
                            Ok(event) => {
                                sink.handle(&SinkNotification::Event(Box::new(event)))
                                    .await?
                            }
                            Err(ReceiveError::ResyncRequired(reason)) => {
                                sink.handle(&SinkNotification::ResyncRequired(reason))
                                    .await?;
                                return Ok(());
                            }
                            Err(ReceiveError::Closed) => return Ok(()),
                        }
                    }
                });
                (name.clone(), task)
            })
            .collect()
    }
}

fn register<T: ?Sized>(
    entries: &mut BTreeMap<String, Arc<T>>,
    kind: &str,
    name: &str,
    extension: Arc<T>,
) -> Result<()> {
    use std::collections::btree_map::Entry;

    match entries.entry(name.to_owned()) {
        Entry::Vacant(entry) => {
            entry.insert(extension);
            Ok(())
        }
        Entry::Occupied(_) => Err(HarnessError::Configuration(format!(
            "{kind} '{name}' is already registered"
        ))),
    }
}
