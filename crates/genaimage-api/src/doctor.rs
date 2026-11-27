use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every check id, in report order.
pub const DOCTOR_CHECK_IDS: [&str; 14] = [
    "server",
    "config",
    "comfyui.reachable",
    "comfyui.version",
    "comfyui.object_info",
    "workflows.nodes",
    "packs.requirements",
    "llm.reachable",
    "llm.text",
    "llm.vision",
    "gpu",
    "disk",
    "ports",
    "event_sinks",
];

/// Outcome of one diagnostic check.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Ok,
    Warning,
    Error,
    /// Not run, or not applicable: an optional service is not configured, or a
    /// check it depends on failed.
    Skipped,
}

/// One diagnostic check. `fix` is present for every status except `ok`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DoctorCheck {
    /// Stable identifier, one of `server`, `config`, `comfyui.reachable`,
    /// `comfyui.version`, `comfyui.object_info`, `workflows.nodes`,
    /// `packs.requirements`, `llm.reachable`, `llm.text`, `llm.vision`, `gpu`, `disk`, `ports` and `event_sinks`.
    pub id: String,
    pub status: DoctorStatus,
    pub summary: String,
    /// Bounded detail lines, such as missing node classes per workflow.
    pub items: Vec<String>,
    /// A concrete action naming the setting, command or page to change.
    pub fix: Option<String>,
}

/// Every check in a fixed order; look checks up by `id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}
