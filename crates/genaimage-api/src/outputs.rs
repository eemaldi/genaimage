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
