//! Schemas for the live event stream's JSON data.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
pub struct EventEnvelope {
    pub bus_id: String,
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunQueued {
        run: EventRun,
    },
    RunStarted {
        run: EventRun,
    },
    RunProgress {
        run: EventRun,
        backend_event: Option<BackendEvent>,
    },
    RunOutput {
        run_id: String,
        output: EventOutput,
    },
    RunCompleted {
        run: EventRun,
    },
    RunFailed {
        run: EventRun,
    },
    RunCancelled {
        run: EventRun,
    },
    RunUnknownSubmission {
        run: EventRun,
    },
    BackendHealthChanged {
        backend: String,
        health: Option<BackendHealth>,
        error: Option<String>,
    },
    SettingsChanged {
        settings: Value,
    },
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct EventRun {
    pub id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub generation_mode: GenerationMode,
    pub status: crate::RunStatus,
    pub parameters: Value,
    pub output_dir: String,
    pub manifest_path: Option<String>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_warnings: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenerationMode {
    Generic,
    InitialConcept,
    Correction,
    Refinement,
    MultiView,
    Delivery,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct EventOutput {
    pub run_id: String,
    pub node_id: String,
    pub kind: String,
    pub source_filename: String,
    pub source_subfolder: String,
    pub source_type: String,
    pub local_path: String,
    pub sha256: String,
    pub byte_size: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct BackendEvent {
    pub event_type: String,
    pub prompt_id: Option<String>,
    pub node_id: Option<String>,
    pub value: Option<u64>,
    pub maximum: Option<u64>,
    pub message: Option<String>,
    pub raw: Value,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct BackendHealth {
    pub system: Value,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ResyncEvent {
    CursorOutOfRange {
        requested: u64,
        oldest: Option<u64>,
        latest: u64,
    },
    Lagged {
        skipped: u64,
    },
    BusChanged {
        requested: String,
        current: String,
    },
    Invalidated {
        id: u64,
    },
}
