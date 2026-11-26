use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which API listed the installed LLM models.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmModelSource {
    /// The Ollama native API (`/api/tags`); pulls are supported.
    Ollama,
    /// Only the OpenAI-compatible `/v1/models` listing; pulls are unsupported.
    Openai,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct InstalledLlmModel {
    pub name: String,
    /// Exact bytes on disk; null when the endpoint does not report it.
    pub size_bytes: Option<u64>,
    pub digest: Option<String>,
    pub modified_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmModelRole {
    Text,
    Vision,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmRoleModelState {
    /// The configured model name is in the installed list (exact match).
    Present,
    /// The configured model name is not installed; nothing is substituted.
    Missing,
    /// No model is configured for this role.
    NotConfigured,
}

/// A configured model role checked against the installed list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct LlmRoleModel {
    pub role: LlmModelRole,
    pub model: Option<String>,
    pub state: LlmRoleModelState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct LlmModelsResponse {
    pub source: LlmModelSource,
    /// Whether `POST /api/v1/models/llm/pulls` can work on this endpoint.
    pub pull_supported: bool,
    /// Set when `pull_supported` is false.
    pub pull_unsupported_reason: Option<String>,
    pub models: Vec<InstalledLlmModel>,
    pub roles: Vec<LlmRoleModel>,
}

/// An explicit request to download one model. Nothing downloads without it.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartModelPullRequest {
    /// `[namespace/]name[:tag]`, for example `llama3.2:3b`. At most 255 bytes.
    #[schema(max_length = 255)]
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelPullState {
    Running,
    /// Ollama reported `success`.
    Succeeded,
    Failed,
    Cancelled,
}

/// A snapshot of one pull. Pulls live in server memory only: after a restart
/// the id is unknown (404), which never means the model was installed. List the
/// models to find out.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ModelPull {
    pub id: String,
    pub name: String,
    pub state: ModelPullState,
    /// Ollama's latest status text, or `starting`, `success`, `failed`, `cancelled`.
    pub status: String,
    /// The layer announced by the latest progress line.
    pub layer: Option<String>,
    /// Exact sum of the layer sizes announced so far; it can grow as layers start.
    pub total_bytes: Option<u64>,
    pub completed_bytes: Option<u64>,
    /// Set when `state` is `failed`, or when `cancelled` has a reason.
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Increases with every update of this pull.
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ModelPullListResponse {
    /// Newest first; only pulls since the server started, with finished ones bounded.
    pub pulls: Vec<ModelPull>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointEnvironment {
    Compose,
    Native,
}

/// Where to put checkpoint files for one kind of installation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct CheckpointLocation {
    pub environment: CheckpointEnvironment,
    pub directory: String,
    pub steps: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct CheckpointsResponse {
    /// `ckpt_name` options of ComfyUI's `CheckpointLoaderSimple`, sorted.
    pub checkpoints: Vec<String>,
    pub locations: Vec<CheckpointLocation>,
    /// ComfyUI only sees new files after a refresh or restart.
    pub refresh_note: String,
}
