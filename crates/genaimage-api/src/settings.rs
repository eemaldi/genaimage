use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::{HealthStatus, ImageBackendHealthReason};

/// Sparse changes using dotted TOML keys. Omitted values remain unchanged.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(transparent)]
pub struct SettingsPatch(pub BTreeMap<String, Value>);

/// Effective settings for the next start, including values already applied live.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SettingsResponse {
    pub settings: BTreeMap<String, Setting>,
    /// Existing selected configuration file or the default target for a first save.
    pub config_file: String,
    /// Keys whose saved and active values differ.
    pub restart_required: Vec<String>,
}

/// Sources reflect environment precedence over the selected file and defaults.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum SettingSource {
    Default,
    File { file: String },
    Env { variable: String },
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct Setting {
    pub value: Value,
    #[serde(flatten)]
    pub source: SettingSource,
    pub read_only: bool,
    /// Present only if this setting still uses a different value at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_value: Option<Value>,
}

/// Connectivity of the proposed settings. Testing does not change any settings.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SettingsTestResponse {
    pub comfyui: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comfyui_reason: Option<ImageBackendHealthReason>,
    pub llm: HealthStatus,
}

/// A field-specific validation issue for an invalid settings proposal.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SettingError {
    pub key: String,
    pub reason: String,
}
