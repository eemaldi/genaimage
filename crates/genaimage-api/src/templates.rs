use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateScope {
    /// Writable, in `paths.templates`.
    User,
    /// Read-only, declared by a pack.
    Pack,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TemplateVariable {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    /// Applied only when the variable is absent from a render request.
    pub default: Option<String>,
}

/// A valid, usable template.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Template {
    /// `user:<id>` or `pack:<pack_id>:<relative path>`.
    #[serde(rename = "ref")]
    pub reference: String,
    pub scope: TemplateScope,
    /// Set for pack templates.
    pub pack_id: Option<String>,
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// Ordered by name.
    pub variables: Vec<TemplateVariable>,
    /// SHA-256 of the template file; send it back to pin a write or a render to this version.
    pub sha256: String,
    pub text: String,
}

/// A template file that is unusable. It never appears in `templates`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TemplateDiagnostic {
    /// Absent when the file name is not a valid template id, or for an omission note.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// The file; empty for an omission note.
    pub path: String,
    pub error: String,
}

/// At most 256 templates and 100 diagnostics; omissions are noted in `diagnostics`.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct TemplateListResponse {
    pub templates: Vec<Template>,
    pub diagnostics: Vec<TemplateDiagnostic>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateVariableInput {
    pub description: Option<String>,
    /// Defaults to `false`. A variable is either required or has a default.
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
}

/// The fields of a template file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateCreateRequest {
    /// `[a-z0-9][a-z0-9_-]{0,63}`; becomes the file name.
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub text: String,
    /// Variable name to definition.
    #[serde(default)]
    pub variables: BTreeMap<String, TemplateVariableInput>,
}

/// Replaces a user template. The id comes from the URL and cannot change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateReplaceRequest {
    /// The `sha256` of the version being replaced.
    pub expected_sha256: String,
    pub title: String,
    pub description: Option<String>,
    pub text: String,
    #[serde(default)]
    pub variables: BTreeMap<String, TemplateVariableInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TemplateDeleteQuery {
    /// The `sha256` of the version being deleted.
    pub expected_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateRenderRequest {
    #[serde(rename = "ref")]
    pub reference: String,
    /// The `sha256` of the template version the user reviewed.
    pub sha256: String,
    /// Variable name to string value. Absent variables take their default; an
    /// empty string is a value.
    #[serde(default)]
    #[schema(value_type = BTreeMap<String, String>)]
    pub variables: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TemplateRenderResponse {
    pub text: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub sha256: String,
}
