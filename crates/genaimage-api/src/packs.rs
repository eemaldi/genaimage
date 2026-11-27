use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::WorkflowDiagnostic;

/// Workflow packs found below `paths.packs`, and discovery problems.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackListResponse {
    pub packs: Vec<PackSummary>,
    /// Broken packs and unreadable declared files; a broken pack never hides the others.
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

/// One valid pack with its declared contents.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackSummary {
    pub id: String,
    pub version: String,
    pub description: String,
    /// The configured pack root that contains this pack.
    pub root: String,
    /// The pack directory.
    pub directory: String,
    /// IDs of the workflows the pack declares and that could be read.
    pub workflow_ids: Vec<String>,
    /// Template paths relative to the pack. Listed only.
    pub templates: Vec<String>,
    pub requires: PackRequirements,
    /// Declared files that are missing or unreadable.
    pub diagnostics: Vec<String>,
}

/// Declared requirements, checked but never installed.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackRequirements {
    pub node_classes: Vec<String>,
    /// Model folder to file names.
    pub models: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackRequirementStatus {
    Available,
    Missing,
    /// Could not be checked; never treat as available.
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackValidationStatus {
    Ok,
    Missing,
    Unknown,
    /// ComfyUI could not be queried; nothing was confirmed.
    Unavailable,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackNodeCheck {
    pub class: String,
    pub status: PackRequirementStatus,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackModelCheck {
    pub folder: String,
    pub name: String,
    pub status: PackRequirementStatus,
    /// The loader input used for the check, as `Class.input`.
    pub loader: Option<String>,
    /// Why the status is `unknown`.
    pub reason: Option<String>,
}

/// Result of checking one pack against ComfyUI `/object_info`.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PackValidationResponse {
    pub pack_id: String,
    pub status: PackValidationStatus,
    /// Why the status is `unavailable`.
    pub reason: Option<String>,
    pub node_classes: Vec<PackNodeCheck>,
    pub models: Vec<PackModelCheck>,
}
