use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::SettingSource;

/// Stable codes explaining the first-run assessment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupReasonCode {
    /// No configuration file exists on disk. Blocking unless the environment owns the endpoints.
    NoConfigFile,
    /// An endpoint in the saved configuration fails validation. Always blocking.
    InvalidConfig,
    /// The environment supplies the ComfyUI endpoint, as in Compose. Advisory.
    EndpointsFromEnvironment,
    /// No LLM endpoint is configured; the assistant is optional. Advisory.
    LlmNotConfigured,
    /// An LLM endpoint is set but no text model is chosen. Advisory.
    LlmTextModelUnset,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SetupReason {
    pub code: SetupReasonCode,
    pub summary: String,
    /// Only blocking reasons make `needs_setup` true.
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Offending settings, as `key: reason`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
}

/// One saved endpoint and where its value comes from.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SetupEndpoint {
    /// Empty when the optional LLM is not configured.
    pub configured_url: String,
    #[serde(flatten)]
    pub source: SettingSource,
}

/// Offline assessment of the saved configuration. It never probes a service.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SetupStatus {
    /// True when any reason is blocking: no config file and the ComfyUI endpoint
    /// is not set by the environment, or the saved configuration is invalid.
    pub needs_setup: bool,
    pub reasons: Vec<SetupReason>,
    pub config_file: String,
    pub config_file_exists: bool,
    /// True when the environment owns `comfyui.base_url`, so saving cannot change it.
    pub endpoints_env_owned: bool,
    /// Setting keys saved but not yet live; a restart applies them.
    pub restart_required: Vec<String>,
    pub comfyui: SetupEndpoint,
    pub llm: SetupEndpoint,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetectedServiceKind {
    Comfyui,
    Llm,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetectStatus {
    /// The endpoint answered with the expected protocol.
    Found,
    /// Nothing answered: connection refused, unresolved name or no answer in time.
    NotFound,
    /// Not probed: the host is not allow-listed or fails endpoint validation.
    Skipped,
    /// Something answered, but not with the expected protocol.
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmFlavour {
    Ollama,
    Openai,
}

/// The outcome for one fixed candidate or the configured URL.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DetectedService {
    pub kind: DetectedServiceKind,
    /// Human label such as `Ollama` or `ComfyUI (Compose)`.
    pub label: String,
    pub url: String,
    /// True when this is the currently saved endpoint.
    pub configured: bool,
    pub status: DetectStatus,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// ComfyUI version reported by `/system_stats`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A hint from the candidate's well-known port, not a protocol check. LLM only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavour: Option<LlmFlavour>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SetupDetectResponse {
    pub services: Vec<DetectedService>,
}
