pub mod artifacts;
pub mod config;
pub mod error;
pub mod extensions;
pub mod image_backend;
pub mod orchestrator;
pub mod persistence;
pub mod workflow;

pub use async_trait::async_trait;
pub use config::{HarnessConfig, ResolvedConfig};
pub use error::{HarnessError, Result};
pub use extensions::{AssistantTool, EventSink, ExtensionRegistry, LlmProvider};
pub use image_backend::ImageBackend;
pub use orchestrator::{Harness, RunProgress, RunRequest};
pub use persistence::{RunRecord, RunStatus};
pub use workflow::{LoadedWorkflow, ParameterKind, WorkflowManifest};
