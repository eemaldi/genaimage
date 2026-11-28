use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Filters completed outputs. Date bounds are inclusive; pass the same filters on each page.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OutputListQuery {
    pub run: Option<String>,
    pub workflow: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    /// An exact, case-sensitive tag from the archived workflow manifest.
    pub tag: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

/// An output and its provenance, without server-local file paths.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct OutputResponse {
    /// Opaque identifier to pass to the media endpoints.
    pub id: String,
    pub run_id: String,
    pub workflow_id: String,
    pub node_id: String,
    pub kind: String,
    pub source_filename: String,
    pub content_type: String,
    pub sha256: String,
    pub byte_size: u64,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct OutputListResponse {
    pub outputs: Vec<OutputResponse>,
    pub next_cursor: Option<String>,
}

/// One append-only annotation from the configured vision model.
///
/// This is model opinion for operator review, not provenance or a verified
/// description of the output.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct OutputAnnotationResponse {
    pub id: i64,
    pub output_id: String,
    pub run_id: String,
    pub kind: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub caption: String,
    pub adherence_critique: String,
    pub tags: Vec<String>,
    /// The complete prompt sent to the loopback model provider.
    pub prompt: String,
    /// The exact concatenated model response used to parse this annotation.
    pub raw_response: String,
    /// The validated checklist when `kind` is `spec_review`; absent for a `vision_review`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_review: Option<SpecReviewResponse>,
}

/// Reviews one output against one exact character spec version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecReviewRequest {
    pub character_id: String,
    /// A whole number naming an existing spec version; the review never uses a newer one.
    pub spec_version: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpecVerdict {
    Met,
    Deviation,
    Unclear,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SpecReviewItemResponse {
    /// An element id of the reviewed spec version.
    pub id: String,
    pub category: crate::SpecElementCategory,
    pub requirement: crate::SpecRequirement,
    pub verdict: SpecVerdict,
    /// At most 500 bytes. `not assessed by the model` marks a mandatory element the model left out.
    pub note: String,
}

/// A model-opinion checklist. It is not an approval or a rejection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SpecReviewResponse {
    pub character_id: String,
    pub spec_version: u32,
    /// Every mandatory element exactly once, then the flexible elements the model assessed.
    pub checklist: Vec<SpecReviewItemResponse>,
    /// What validation dropped from the model's answer, such as unknown ids or invalid verdicts.
    pub dropped: Vec<String>,
    /// At most 1,000 bytes.
    pub summary: String,
    pub label: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct OutputAnnotationListResponse {
    pub annotations: Vec<OutputAnnotationResponse>,
}

/// Longest thumbnail edge in pixels: 128, 256, 512 or 1024.
#[derive(Debug, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ThumbnailQuery {
    #[param(value_type = ThumbnailSizeSchema)]
    #[schema(value_type = ThumbnailSizeSchema)]
    pub size: u32,
}

/// The supported longest-edge sizes for generated thumbnails.
pub struct ThumbnailSizeSchema;

impl utoipa::PartialSchema for ThumbnailSizeSchema {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::Integer)
            .enum_values(Some([128_u32, 256, 512, 1024]))
            .into()
    }
}
impl ToSchema for ThumbnailSizeSchema {}
