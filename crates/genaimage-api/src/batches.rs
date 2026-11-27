use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use utoipa::ToSchema;

use crate::{RunStatus, WorkflowInheritedInput};

/// A pinned batch of runs: a base set of explicit values plus one sweep. Each
/// item becomes an ordinary run. Used by both preview and submission.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitBatchRequest {
    /// Manifest ID, not a local path.
    pub workflow: String,
    /// Revision from the reviewed workflow. Required: batches are always pinned.
    pub workflow_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Values shared by every item. A swept parameter must not appear here.
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    pub sweep: BatchSweep,
    /// The largest item count the caller accepts. The request is rejected, never
    /// truncated, if the sweep expands to more items than this or than the
    /// server limit.
    pub max_items: usize,
}

/// Exactly one of `seeds` or `grid`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchSweep {
    /// An explicit seed list. The server never generates seeds.
    Seeds(SeedSweep),
    /// Parameter name to its values. Items are the cartesian product: names are
    /// sorted by byte order and the last name varies fastest. A name must be
    /// listed once; the server rejects a repeated name with `batch-invalid`.
    Grid(BatchGrid),
}

/// The lists of a grid sweep, keyed by parameter name. A name that the request
/// listed more than once is recorded in `repeated` so the server can reject it,
/// instead of one list silently replacing another.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct BatchGrid {
    pub axes: BTreeMap<String, Vec<Value>>,
    #[serde(skip)]
    pub repeated: Vec<String>,
}

impl utoipa::PartialSchema for BatchGrid {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, SchemaType, Type};
        let values =
            ArrayBuilder::new().items(ObjectBuilder::new().schema_type(SchemaType::AnyValue));
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .additional_properties(Some(values))
            .into()
    }
}

impl ToSchema for BatchGrid {}

impl<'de> Deserialize<'de> for BatchGrid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Axes;

        impl<'de> Visitor<'de> for Axes {
            type Value = BatchGrid;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of parameter names to lists of values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BatchGrid, A::Error> {
                let mut grid = BatchGrid::default();
                while let Some((name, values)) = map.next_entry::<String, Vec<Value>>()? {
                    if grid.axes.insert(name.clone(), values).is_some()
                        && !grid.repeated.contains(&name)
                    {
                        grid.repeated.push(name);
                    }
                }
                Ok(grid)
            }
        }

        deserializer.deserialize_map(Axes)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SeedSweep {
    /// A workflow parameter of kind `seed`.
    pub parameter: String,
    /// Distinct, exact integers up to 2^64 - 1.
    pub values: Vec<Value>,
}

/// One item as it would be submitted. Nothing has been sent.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchPreviewItem {
    pub index: usize,
    /// Exactly what the item's run request carries: shared values plus swept values.
    pub parameters: BTreeMap<String, Value>,
    /// The swept values alone.
    pub sweep_values: BTreeMap<String, Value>,
    /// The values the run will record: preset, explicit values and workflow defaults.
    pub effective_parameters: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchPreviewResponse {
    pub item_count: usize,
    /// Echo of the request's `max_items`.
    pub max_items: usize,
    /// The server limit on items per batch.
    pub max_items_cap: usize,
    pub workflow_revision: String,
    /// Graph inputs left untouched by binding, not submitted values. Every item
    /// sets the same parameters, so they are the same for all items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_inputs: Vec<WorkflowInheritedInput>,
    pub items: Vec<BatchPreviewItem>,
}

/// State of the submission driver and its outcome, not of the runs. It never
/// reports success while an item is unknown or unsent.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    /// Items are still being submitted.
    Submitting,
    /// Some items were never submitted. Resume the batch to submit them.
    Interrupted,
    /// Every item has a run accepted by ComfyUI. Run outcomes are separate.
    Submitted,
    /// Some items failed or are unknown, and none are pending.
    Partial,
    /// No item was accepted.
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchItemSubmission {
    Pending,
    Submitted,
    /// No prompt was sent for it, and `error` says why. Without a `run_id` the
    /// item was rejected before a run was recorded, and resuming retries it.
    /// With a `run_id` it failed while the run was being prepared, which is
    /// final: resuming never retries it.
    Failed,
    /// Never attempted. Resuming submits it.
    NotSubmitted,
    /// ComfyUI may have accepted the prompt. Reconcile the run; never resubmit.
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct BatchCounts {
    pub pending: usize,
    pub submitted: usize,
    pub failed: usize,
    pub not_submitted: usize,
    pub outcome_unknown: usize,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchItemResponse {
    pub index: usize,
    pub parameters: BTreeMap<String, Value>,
    pub sweep_values: BTreeMap<String, Value>,
    pub run_id: Option<String>,
    pub submission: BatchItemSubmission,
    pub error: Option<String>,
    /// The run's own status. Success is shown only once the backend confirmed outputs.
    pub run_status: Option<RunStatus>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchResponse {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub workflow: String,
    pub workflow_revision: String,
    pub item_count: usize,
    pub state: BatchState,
    pub counts: BatchCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The values shared by every item.
    pub parameters: BTreeMap<String, Value>,
    pub sweep: BatchSweep,
    pub max_items: usize,
    pub items: Vec<BatchItemResponse>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchSummaryResponse {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub workflow: String,
    pub workflow_revision: String,
    pub item_count: usize,
    pub state: BatchState,
    pub counts: BatchCounts,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct BatchListQuery {
    pub workflow: Option<String>,
    pub cursor: Option<String>,
    /// Defaults to 50; maximum 200.
    pub limit: Option<usize>,
}

/// Batches newest first, and an opaque token for the next page, if any.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchListResponse {
    pub batches: Vec<BatchSummaryResponse>,
    pub next_cursor: Option<String>,
}

/// An item rejected by validation, with every problem found in it.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct BatchItemProblem {
    pub index: usize,
    pub errors: Vec<crate::WorkflowParameterError>,
}
