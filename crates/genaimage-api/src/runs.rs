use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// Explicit bindings for a workflow configured in the local workspace.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitRunRequest {
    /// Manifest ID, not a local path.
    pub workflow: String,
    /// Revision from the reviewed workflow. Omit for unpinned submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    /// A checkpoint profile of the workflow's pack. It applies only when named,
    /// and only if the effective checkpoint is one of its exact names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_profile: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    /// Optional output version. Existing artefacts are never overwritten.
    #[serde(default)]
    pub version: Option<String>,
}

/// Durable lifecycle state of a run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created,
    Submitting,
    /// ComfyUI may have accepted the prompt. Reconcile; do not resubmit it.
    UnknownSubmission,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelling,
    Cancelled,
}

/// Current snapshot of a run, including its effective parameters.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct RunResponse {
    pub id: String,
    pub prompt_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    /// Revision derived from the run's recorded graph and manifest fingerprints, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<String>,
    /// The source run ID when this run was created by a re-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_of: Option<String>,
    pub status: RunStatus,
    pub parameters: Value,
    pub error: Option<String>,
    /// Node diagnostics from an accepted submission, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_warnings: Option<Value>,
    /// The batch that created this run; absent for runs submitted on their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// The run's position in its batch, starting at 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_index: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Optional filters and paging for listing runs, newest first.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct RunListQuery {
    pub status: Option<RunStatus>,
    pub workflow: Option<String>,
    /// Only runs created by this batch.
    pub batch: Option<String>,
    /// Inclusive RFC 3339 instant.
    pub created_from: Option<DateTime<Utc>>,
    /// Inclusive RFC 3339 instant.
    pub created_to: Option<DateTime<Utc>>,
    pub cursor: Option<String>,
    /// Defaults to 50; maximum 200.
    pub limit: Option<usize>,
}

/// A page of matching runs and an opaque token for the next page, if any.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct RunListResponse {
    pub runs: Vec<RunResponse>,
    pub next_cursor: Option<String>,
}

/// A persisted lifecycle or progress event, independent of the live event bus.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct RunEventResponse {
    pub id: i64,
    pub run_id: String,
    pub event_type: String,
    pub message: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

/// Bounds the latest events returned; defaults to 50, maximum 200.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct RunEventsQuery {
    pub limit: Option<usize>,
}

/// Latest persisted events in ascending event ID order.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct RunEventsResponse {
    pub events: Vec<RunEventResponse>,
}

/// What reconciliation established, independently of the run status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// The run was already terminal or reached a terminal state.
    Terminal,
    /// The prompt was found pending or running in the backend queue.
    Active,
    /// A live submission owner still holds the run.
    InProgressElsewhere,
    /// The prompt was absent from queue and history after the settle window.
    /// ComfyUI cannot prove absence; very late admission is still possible.
    NotFound,
    /// The backend evidence did not settle the run's state.
    StillAmbiguous,
}

/// A refreshed run snapshot with the outcome of this reconciliation.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct ReconcileRunResponse {
    #[serde(flatten)]
    pub run: RunResponse,
    #[serde(default = "inconclusive_outcome")]
    pub outcome: ReconcileOutcome,
    /// Earliest time to check again when an absent submission is still settling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settles_at: Option<DateTime<Utc>>,
}

fn inconclusive_outcome() -> ReconcileOutcome {
    ReconcileOutcome::StillAmbiguous
}

/// Current snapshots of the runs examined during bulk reconciliation.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct ReconcileRunsResponse {
    pub runs: Vec<RunResponse>,
}

/// Current snapshots and outcomes of the runs examined during bulk reconciliation.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct ReconcileRunsWithOutcomesResponse {
    pub runs: Vec<ReconcileRunResponse>,
}
