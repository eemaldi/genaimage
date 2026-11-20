pub mod artifacts;
pub mod comfyui;
pub mod config;
pub mod error;
pub mod gates;
pub mod orchestrator;
pub mod persistence;
pub mod workflow;

pub use config::{HarnessConfig, ResolvedConfig};
pub use error::{HarnessError, Result};
pub use gates::{ApprovalStatus, CharacterState, GateId};
pub use orchestrator::{Harness, RunProgress, RunRequest};
pub use persistence::{RunRecord, RunStatus};
pub use workflow::{LoadedWorkflow, ParameterKind, WorkflowManifest};
