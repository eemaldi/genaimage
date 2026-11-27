use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// An API-format ComfyUI graph to analyse. Nothing is written.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowImportRequest {
    /// The ComfyUI API-format graph: an object keyed by node ID, each node
    /// with `class_type` and `inputs`. UI-format exports are rejected.
    #[schema(value_type = Object)]
    pub graph: Value,
    /// Optional display name. It only seeds the suggested `title` and `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A proposed manifest and what the importer did and did not recognise.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowImportProposal {
    /// SHA-256 of the normalised graph bytes. Send it back unchanged with the graph on confirmation.
    pub graph_sha256: String,
    /// A manifest the user may edit before confirming. It already passes validation.
    pub manifest: WorkflowImportManifest,
    /// Nodes with import rules, in node order, with the parameters they received.
    pub recognised: Vec<WorkflowImportRecognisedNode>,
    /// Nodes without import rules. Their inputs keep the workflow values.
    pub unknown_nodes: Vec<WorkflowImportUnknownNode>,
    /// Inputs of recognised nodes that no parameter controls, including connections.
    pub unbound_inputs: Vec<WorkflowImportUnboundInput>,
    pub warnings: Vec<String>,
}

/// A version 2 manifest as proposed or confirmed. Unknown fields are rejected.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowImportManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub title: String,
    /// Must be `api-workflow.json`; the server chooses the file name.
    pub workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, WorkflowImportParameter>,
    #[serde(default)]
    pub presets: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    pub outputs: Vec<WorkflowImportOutput>,
}

/// A parameter binding. Numeric values are exact JSON numbers.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowImportParameter {
    pub node: String,
    pub input: String,
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
    #[serde(
        default,
        deserialize_with = "keep_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = f64, required = false)]
    pub minimum: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = f64, required = false)]
    pub maximum: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowImportOutput {
    pub node: String,
    #[serde(default = "image_kind")]
    pub kind: String,
}

/// Keeps a JSON `null` as `Some(Null)` so the server can refuse it explicitly.
fn keep_explicit_null<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

fn image_kind() -> String {
    "image".to_owned()
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowImportRecognisedNode {
    pub node: String,
    pub class_type: String,
    pub bound_inputs: Vec<WorkflowImportBoundInput>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowImportBoundInput {
    pub input: String,
    /// The manifest parameter that controls this input.
    pub parameter: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowImportUnknownNode {
    pub node: String,
    pub class_type: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WorkflowImportUnboundInput {
    pub node: String,
    pub class_type: String,
    pub input: String,
    /// `true` when the input is a connection to another node's output.
    pub linked: bool,
    pub reason: String,
}

/// The graph and the (possibly edited) manifest to write as a new workflow.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowImportConfirmRequest {
    #[schema(value_type = Object)]
    pub graph: Value,
    /// The `graph_sha256` of the proposal for this graph.
    pub graph_sha256: String,
    pub manifest: WorkflowImportManifest,
}

/// A workflow written to the user workflow root and verified loadable.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct WorkflowImportResponse {
    pub id: String,
    /// Opaque revision of the written graph and manifest.
    pub revision: String,
    pub manifest_path: String,
    /// Always `user`.
    pub source: String,
}
