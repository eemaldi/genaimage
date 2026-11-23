//! Extension contracts and explicit application wiring. No runtime plugin loading.

use std::{collections::BTreeMap, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::ComfyUiProfile, HarnessError, ImageBackend, ResolvedConfig, Result, RunProgress,
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
#[serde(rename_all = "snake_case")]
pub enum ChatDelta {
    Text(String),
    /// A complete call with parsed arguments, not an incremental JSON fragment.
    ToolCall(ToolCall),
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

/// The tool runtime must validate arguments against the schema and obtain any
/// required user confirmation before invoking the tool. `requires_confirmation`
/// is metadata, not enforcement. An assistant must never submit a run without
/// explicit user approval of the effective proposal, regardless of tool metadata.
#[async_trait]
pub trait AssistantTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn json_schema(&self) -> Value;
    fn requires_confirmation(&self) -> bool;
    async fn invoke(&self, arguments: Value) -> Result<Value>;
}

/// Consumes a run progress update. This contract does not dispatch events or
/// define a bus, delivery guarantees or persistence policy. When #21 adds a
/// typed event envelope, use it here and keep sink handling off the publish path.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn handle(&self, event: &RunProgress) -> Result<()>;
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
        register(&mut self.assistant_tools, "assistant tool", &name, tool)
    }

    pub fn assistant_tool(&self, name: &str) -> Option<Arc<dyn AssistantTool>> {
        self.assistant_tools.get(name).cloned()
    }

    pub fn register_event_sink(&mut self, name: &str, sink: Arc<dyn EventSink>) -> Result<()> {
        register(&mut self.event_sinks, "event sink", name, sink)
    }

    pub fn event_sink(&self, name: &str) -> Option<Arc<dyn EventSink>> {
        self.event_sinks.get(name).cloned()
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
