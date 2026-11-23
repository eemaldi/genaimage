//! The operations used by the current harness, to be formalised as `ImageBackend` in #19.

use std::{future::Future, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::Result;

pub trait ComfyUiTransport: Send + Sync {
    fn base_url(&self) -> &str;
    fn health(&self) -> impl Future<Output = Result<ComfyUiHealth>> + Send;
    fn object_info(&self) -> impl Future<Output = Result<Value>> + Send;
    fn queue(&self) -> impl Future<Output = Result<QueueSnapshot>> + Send;
    fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> impl Future<Output = Result<PromptResponse>> + Send;
    fn history(&self, prompt_id: &str) -> impl Future<Output = Result<HistorySnapshot>> + Send;
    fn delete_queued(&self, prompt_id: &str) -> impl Future<Output = Result<Value>> + Send;
    fn interrupt(&self) -> impl Future<Output = Result<Value>> + Send;
    fn upload_image(&self, path: &Path) -> impl Future<Output = Result<UploadResponse>> + Send;
    fn download_output(
        &self,
        output: &OutputReference,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn monitor_events(&self, client_id: &str) -> Result<EventMonitor>;
}

#[derive(Clone, Debug, Serialize)]
pub struct ComfyUiHealth {
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
        self.running.contains(&prompt_id.to_owned()) || self.pending.contains(&prompt_id.to_owned())
    }

    pub fn is_running(&self, prompt_id: &str) -> bool {
        self.running.contains(&prompt_id.to_owned())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadResponse {
    pub name: String,
    pub subfolder: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComfyUiEvent {
    pub event_type: String,
    pub prompt_id: Option<String>,
    pub node_id: Option<String>,
    pub value: Option<u64>,
    pub maximum: Option<u64>,
    pub message: Option<String>,
    pub raw: Value,
}

pub struct EventMonitor {
    pub receiver: mpsc::Receiver<ComfyUiEvent>,
    task: JoinHandle<()>,
}

impl EventMonitor {
    pub fn new(receiver: mpsc::Receiver<ComfyUiEvent>, task: JoinHandle<()>) -> Self {
        Self { receiver, task }
    }

    pub fn abort(&self) {
        self.task.abort();
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
    pub complete: bool,
    pub success: bool,
    pub outputs: Vec<OutputReference>,
    pub raw: Value,
}
