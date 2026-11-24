pub mod artifacts;
pub mod assistant;
pub mod config;
pub mod error;
pub mod event;
pub mod extensions;
pub mod image_backend;
pub mod llm_roles;
pub mod media;
pub mod orchestrator;
pub mod persistence;
#[doc(hidden)]
pub mod safe_file;
pub mod uploads;
pub mod workflow;

pub use async_trait::async_trait;
pub use config::{HarnessConfig, ResolvedConfig};
pub use error::{HarnessError, Result};
pub use event::{
    Event, EventBus, EventCursor, EventEnvelope, EventSubscription, ProgressSubscription,
    ReceiveError, ResyncReason,
};
pub use extensions::{
    AssistantTool, ChatContent, ChatDelta, ChatMessage, ChatRequest, ChatStream, EventSink,
    ExtensionRegistry, LlmModel, LlmProvider, SinkNotification, ToolCall, ToolDefinition,
};
pub use image_backend::ImageBackend;
pub use llm_roles::{
    AssistantUnavailable, LlmCapability, LlmHealthSnapshot, LlmModelSnapshot, LlmRole,
    LlmRoleBinding, LlmRoleHealth, LlmRoleStatus, LlmRoles, LLM_PROVIDER,
};
pub use orchestrator::{Harness, KeyedSubmission, RunRequest};
pub use persistence::{RunRecord, RunStatus};
pub use workflow::{LoadedWorkflow, ParameterKind, WorkflowManifest};
