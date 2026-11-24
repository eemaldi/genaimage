//! Shared wire types for the local API.

#[cfg(feature = "client")]
pub mod client;

mod events;
pub use events::*;

mod settings;
pub use settings::*;

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct HealthResponse {
    #[serde(rename = "self")]
    pub server: HealthStatus,
    pub image_backend: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_backend_reason: Option<ImageBackendHealthReason>,
    pub llm: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_text: Option<LlmRoleHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_vision: Option<LlmRoleHealth>,
    /// Recovery guidance for services; absent on older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<HealthHints>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct HealthHints {
    #[serde(rename = "self")]
    pub server: String,
    pub image_backend: String,
    pub llm: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Unavailable,
    NotConfigured,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct LlmRoleHealth {
    pub status: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct LlmModel {
    pub id: String,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageBackendHealthReason {
    ProbeFailed,
    Timeout,
}

/// RFC 9457 problem details with a GenAImage recovery hint.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub recovery: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<SettingError>,
}

/// A workflow catalogue and non-fatal discovery errors.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowListResponse {
    pub workflows: Vec<WorkflowSummary>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

/// Descriptive catalogue information for one workflow.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub category: Option<String>,
    pub thumbnail_url: Option<String>,
    pub version: String,
    /// Opaque revision of the API graph and manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(required = true, value_type = String)]
    pub revision: Option<String>,
}

/// A manifest that could not be loaded during discovery.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowDiagnostic {
    pub path: String,
    pub error: String,
}

/// A version 2 workflow manifest, including its parameter UI hints.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowDetails {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    /// Opaque revision of the API graph and manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(required = true, value_type = String)]
    pub revision: Option<String>,
    pub title: String,
    pub workflow: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub category: Option<String>,
    pub thumbnail: Option<String>,
    pub parameters: std::collections::BTreeMap<String, WorkflowParameter>,
    pub presets:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, serde_json::Value>>,
    pub outputs: Vec<WorkflowOutput>,
}

fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A workflow parameter, its binding constraints and advisory UI hints.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowParameter {
    pub node: String,
    pub input: String,
    pub kind: String,
    pub required: bool,
    /// Absent if unknown, null if defaulted, or the value declared in the manifest.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub declared_required: Option<Option<bool>>,
    pub description: String,
    pub default: Option<serde_json::Value>,
    pub allowed_values: Option<Vec<serde_json::Value>>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub widget: Option<String>,
    pub step: Option<f64>,
    pub options: Option<Vec<serde_json::Value>>,
    pub group: Option<String>,
    pub advanced: Option<bool>,
    pub placeholder: Option<String>,
}

/// A declared workflow output.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowOutput {
    pub node: String,
    pub kind: String,
    /// Absent if unknown, null if defaulted, or the value declared in the manifest.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub declared_kind: Option<Option<String>>,
}

/// Parameter overrides and an optional manifest preset to validate.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowValidationRequest {
    /// Revision from the reviewed workflow. Omit for unpinned validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The result of validating bindings and checking backend node availability.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowValidationResponse {
    /// Opaque revision of the API graph and manifest that were validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(required = true, value_type = String)]
    pub revision: Option<String>,
    pub valid: bool,
    pub parameter_errors: Vec<WorkflowParameterError>,
    /// Graph inputs left untouched by binding, not submitted parameter values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_inputs: Vec<WorkflowInheritedInput>,
    pub node_check: WorkflowNodeCheck,
}

/// The graph value an unbound optional parameter leaves in place.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowInheritedInput {
    pub parameter: String,
    pub node: String,
    pub input: String,
    pub source: WorkflowInheritedSource,
}

/// The exact graph literal (JSON text), or a ComfyUI node output connection.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowInheritedSource {
    Literal { json: String },
    Connection { node: String, output: u64 },
}

/// A parameter binding failure.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowParameterError {
    pub parameter: String,
    pub reason: String,
}

/// Node check state. An unavailable check never implies a valid workflow.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowNodeCheck {
    pub status: WorkflowNodeCheckStatus,
    pub missing_classes: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeCheckStatus {
    Available,
    Missing,
    Unavailable,
}

mod outputs;
pub use outputs::*;

mod runs;
pub use runs::*;

/// Metadata for a stored reference image. Use `upload_id` as the string value
/// of an `image_upload` workflow parameter in the runs and validation APIs.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct UploadResponse {
    /// Content-addressed identifier in the form `sha256:<lowercase hex digest>`.
    pub upload_id: String,
    pub sha256: String,
    pub byte_size: u64,
    /// Media type detected from the file signature, not supplied by the client.
    pub media_type: String,
    /// Sanitised basename from the first accepted upload of these bytes.
    pub original_filename: String,
}
