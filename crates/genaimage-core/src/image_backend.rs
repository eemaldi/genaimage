//! Image generation backend contract and transport-neutral run data.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::Result;

/// Executes an explicitly bound workflow; never silently substitute a model,
/// node, seed, image, prompt or setting.
///
/// Submission is attempted once per run. The caller persists a correlation ID
/// before submitting. If acceptance is uncertain (for example, the response
/// is lost), the backend must not retry the submission, and the caller must
/// reconcile that ID against queue and history. Absence from both is not proof
/// of rejection. Only an acknowledged submission may return a different ID,
/// and that ID must identify the same, unmodified submission.
#[async_trait]
pub trait ImageBackend: Send + Sync {
    /// Display name for diagnostics, not a protocol or implementation selector.
    fn name(&self) -> &str;

    /// Endpoint recorded in run provenance.
    fn base_url(&self) -> &str;
    /// Update timing without changing the backend identity or dropping its DNS
    /// pin. Backends that cannot guarantee this defer timing changes to restart.
    fn with_timing(
        &self,
        _request_timeout_seconds: u64,
        _websocket_reconnect_seconds: u64,
    ) -> Option<Arc<dyn ImageBackend>> {
        None
    }

    async fn health(&self) -> Result<BackendHealth>;

    /// Available workflow node classes, keyed by the class names used in workflows.
    async fn object_info(&self) -> Result<Value>;
    async fn queue(&self) -> Result<QueueSnapshot>;
    async fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> Result<PromptResponse>;
    /// Return evidence for the requested prompt only. A missing entry is unresolved.
    async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot>;
    /// Request deletion. Acknowledgement alone does not confirm cancellation.
    /// Return `CancellationNotSent` or `CancellationRejected` only for a definite
    /// outcome; other errors are treated as an unconfirmed request.
    async fn delete_queued(&self, prompt_id: &str) -> Result<Value>;

    /// Check targeted interruption support without sending a mutation.
    async fn prepare_interrupt(&self, _prompt_id: &str) -> Result<()> {
        Ok(())
    }

    /// Request interruption of only the named prompt after `prepare_interrupt` succeeds.
    /// Never fall back to an unscoped interrupt. Uses the same error contract as deletion.
    async fn interrupt(&self, prompt_id: &str) -> Result<Value>;
    async fn upload_image(&self, path: &Path) -> Result<UploadResponse>;
    async fn download_output(&self, output: &OutputReference) -> Result<Vec<u8>>;

    /// Observe progress for this client. Events are advisory; reconcile with
    /// queue and history to establish the run state and collect outputs.
    async fn monitor_events(&self, client_id: &str) -> Result<EventMonitor>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BackendHealth {
    pub system: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PromptResponse {
    pub prompt_id: String,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub node_errors: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueueSnapshot {
    pub running: Vec<String>,
    pub pending: Vec<String>,
    pub raw: Value,
}

impl QueueSnapshot {
    pub fn contains(&self, prompt_id: &str) -> bool {
        self.is_running(prompt_id) || self.pending.iter().any(|id| id == prompt_id)
    }

    pub fn is_running(&self, prompt_id: &str) -> bool {
        self.running.iter().any(|id| id == prompt_id)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadResponse {
    pub name: String,
    pub subfolder: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BackendEvent {
    pub event_type: String,
    pub prompt_id: Option<String>,
    pub node_id: Option<String>,
    pub value: Option<u64>,
    pub maximum: Option<u64>,
    pub message: Option<String>,
    pub raw: Value,
}

pub struct EventMonitor {
    receiver: mpsc::Receiver<BackendEvent>,
    task: JoinHandle<()>,
}

impl EventMonitor {
    pub fn new(receiver: mpsc::Receiver<BackendEvent>, task: JoinHandle<()>) -> Self {
        Self { receiver, task }
    }

    pub async fn recv(&mut self) -> Option<BackendEvent> {
        self.receiver.recv().await
    }
}

impl Drop for EventMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputReference {
    pub node_id: String,
    pub kind: String,
    pub filename: String,
    pub subfolder: String,
    pub output_type: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistorySnapshot {
    pub prompt_id: String,
    pub outcome: HistoryOutcome,
    pub outputs: Vec<OutputReference>,
    pub raw: Value,
}

/// What the backend can establish from prompt-specific history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOutcome {
    /// History is absent, incomplete, or does not establish a terminal outcome.
    #[default]
    Unresolved,
    Completed,
    Failed,
    /// Explicit evidence that this prompt was interrupted or removed before execution.
    /// Absence from the queue or an acknowledgement of a cancellation request is not evidence.
    Cancelled,
}
