use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{OutputResponse, RunStatus};

/// The spec version of a character that a run is submitted against.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CharacterLink {
    /// A character ID (`char-…`).
    pub id: String,
    /// An existing spec version of that character. A whole number, never a
    /// string or `2.0`.
    pub spec_version: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpecElementCategory {
    Silhouette,
    Proportions,
    Anatomy,
    Clothing,
    Equipment,
    Materials,
    Colour,
    Other,
}

/// How strictly a generation must honour a spec element.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpecRequirement {
    Mandatory,
    Flexible,
    Unspecified,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CharacterView {
    Front,
    Side,
    Back,
    ThreeQuarter,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecElement {
    /// A stable slug, unique within one spec: lowercase letters, digits, `-`
    /// and `_`, at most 48 bytes.
    pub id: String,
    pub category: SpecElementCategory,
    /// At most 1,000 bytes.
    pub description: String,
    pub requirement: SpecRequirement,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteEntry {
    /// At most 64 bytes, one line.
    pub name: String,
    /// `#rrggbb`, kept exactly as given.
    #[serde(default)]
    pub hex: Option<String>,
    /// At most 64 bytes, one line.
    #[serde(default)]
    pub role: Option<String>,
}

/// The content of one spec version. Rejected whole when any part is invalid;
/// nothing is trimmed, clamped or dropped.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CharacterSpec {
    /// At most 2,000 bytes.
    pub summary: String,
    /// At most 64 elements.
    #[serde(default)]
    pub elements: Vec<SpecElement>,
    /// At most 32 entries.
    #[serde(default)]
    pub palette: Vec<PaletteEntry>,
    /// Required views, without repeats.
    #[serde(default)]
    pub views: Vec<CharacterView>,
    /// At most 4,000 bytes.
    #[serde(default)]
    pub style_notes: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct CharacterResponse {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// `null` until the first spec version exists.
    pub latest_spec_version: Option<u32>,
}

/// Newest characters first. A page without `next_cursor` is the last one.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct CharacterListResponse {
    pub characters: Vec<CharacterResponse>,
    pub next_cursor: Option<String>,
}

/// Creates a character and, when `spec` is given, its version 1 in one write.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCharacterRequest {
    /// One line, at most 200 bytes.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<CharacterSpec>,
    /// Recorded with version 1, at most 1,000 bytes. Rejected when `spec` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct CreateCharacterResponse {
    pub character: CharacterResponse,
    /// Version 1 when the request carried a spec.
    pub spec_version: Option<SpecVersionResponse>,
}

/// Appends a spec version. Versions are never edited.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSpecVersionRequest {
    /// The latest version the author saw; omit it only for the first version.
    /// A stale value is refused with 409 and `latest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<u32>,
    pub spec: CharacterSpec,
    /// At most 1,000 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SpecVersionResponse {
    pub character_id: String,
    /// 1, 2, 3 and so on, without gaps.
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub based_on: Option<u32>,
    pub spec: CharacterSpec,
    pub note: Option<String>,
}

/// Newest version first.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct SpecVersionListResponse {
    pub versions: Vec<SpecVersionResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Sketch,
    Reference,
    Mask,
}

/// Attaches an existing upload to a character.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddReferenceRequest {
    /// An upload ID from `POST /uploads`, at most 128 bytes.
    pub upload_id: String,
    pub role: ReferenceRole,
    /// At most 1,000 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ReferenceResponse {
    pub id: String,
    pub character_id: String,
    pub upload_id: String,
    pub role: ReferenceRole,
    pub note: Option<String>,
    pub added_at: DateTime<Utc>,
    /// Set once by a soft removal; the record is kept.
    pub removed_at: Option<DateTime<Utc>>,
}

/// Newest reference first.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct ReferenceListResponse {
    pub references: Vec<ReferenceResponse>,
    pub next_cursor: Option<String>,
}

/// `cleared` undoes the previous decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CharacterDecision {
    Approved,
    Rejected,
    Cleared,
}

/// Appends a decision about an output of a completed run.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordDecisionRequest {
    /// An output ID exactly as the outputs API reports it (`{run_id}:{row}`).
    pub output_id: String,
    /// The spec version the author judged the output against. A whole number.
    pub spec_version: u32,
    pub decision: CharacterDecision,
    /// At most 1,000 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DecisionResponse {
    pub id: String,
    pub character_id: String,
    pub output_id: String,
    pub spec_version: u32,
    pub decision: CharacterDecision,
    pub note: Option<String>,
    pub decided_at: DateTime<Utc>,
}

/// Every decision, including `cleared` ones, newest first.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct DecisionListResponse {
    pub decisions: Vec<DecisionResponse>,
    pub next_cursor: Option<String>,
}

/// A current decision and the output it is about.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CharacterOutputResponse {
    pub decision: DecisionResponse,
    pub output: OutputResponse,
}

/// Current states, newest decision first. Cleared outputs are not listed.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CharacterOutputListResponse {
    pub outputs: Vec<CharacterOutputResponse>,
    pub next_cursor: Option<String>,
}

/// The decisions that count as a current state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurrentDecisionState {
    Approved,
    Rejected,
}

/// Newest first. Pass the same filters on each page.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CharacterListQuery {
    /// Defaults to 50; 1 to 200.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ReferenceListQuery {
    /// Also list soft-removed references. Defaults to false.
    pub include_removed: Option<bool>,
    /// Defaults to 50; 1 to 200.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct DecisionListQuery {
    /// Only decisions about this output.
    pub output_id: Option<String>,
    /// Only decisions made against this spec version.
    pub spec_version: Option<u32>,
    /// Defaults to 50; 1 to 200.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CharacterOutputsQuery {
    /// Only outputs whose current state is this. Omit for both.
    pub state: Option<CurrentDecisionState>,
    /// Only states decided against this spec version.
    pub spec_version: Option<u32>,
    /// Defaults to 50; 1 to 200.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CharacterRunsQuery {
    /// Only runs submitted against this spec version.
    pub spec_version: Option<u32>,
    pub status: Option<RunStatus>,
    /// Defaults to 50; 1 to 200.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}
