use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Explicit bindings for a workflow configured in the local workspace.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitRunRequest {
    /// Manifest ID, not a local path.
    pub workflow: String,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    /// Optional output version. Existing artefacts are never overwritten.
    #[serde(default)]
    pub version: Option<String>,
}

/// Durable lifecycle state of a run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(Debug, Deserialize, Serialize)]
pub struct RunResponse {
    pub id: String,
    pub prompt_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub status: RunStatus,
    pub parameters: Value,
    pub error: Option<String>,
    /// Node diagnostics from an accepted submission, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_warnings: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Optional filters and paging for listing runs, newest first.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunListQuery {
    pub status: Option<RunStatus>,
    pub workflow: Option<String>,
    /// Inclusive RFC 3339 instant.
    pub created_from: Option<DateTime<Utc>>,
    /// Inclusive RFC 3339 instant.
    pub created_to: Option<DateTime<Utc>>,
    pub cursor: Option<String>,
    /// Defaults to 50; maximum 200.
    pub limit: Option<usize>,
}

/// A page of matching runs and an opaque token for the next page, if any.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunListResponse {
    pub runs: Vec<RunResponse>,
    pub next_cursor: Option<String>,
}

/// A persisted lifecycle or progress event, independent of the live event bus.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunEventResponse {
    pub id: i64,
    pub run_id: String,
    pub event_type: String,
    pub message: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

/// Bounds the latest events returned; defaults to 50, maximum 200.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunEventsQuery {
    pub limit: Option<usize>,
}

/// Latest persisted events in ascending event ID order.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunEventsResponse {
    pub events: Vec<RunEventResponse>,
}

/// Current snapshots of the runs examined during bulk reconciliation.
#[derive(Debug, Deserialize, Serialize)]
pub struct ReconcileRunsResponse {
    pub runs: Vec<RunResponse>,
}
