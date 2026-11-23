use std::{path::PathBuf, sync::RwLock};

use genaimage_core::{ApprovalStatus, GateId, Harness, HarnessError, RunRecord, RunRequest};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

struct DesktopState {
    workspace: RwLock<PathBuf>,
}

impl DesktopState {
    fn workspace(&self) -> PathBuf {
        self.workspace
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn harness(&self) -> Result<Harness, HarnessError> {
        Harness::open(&self.workspace())
    }
}

#[derive(Deserialize)]
struct GateDecisionRequest {
    character_id: String,
    gate: GateId,
    approver: String,
    note: String,
    approved: bool,
}

#[tauri::command]
fn current_workspace(state: State<'_, DesktopState>) -> String {
    state.workspace().display().to_string()
}

#[tauri::command]
fn set_workspace(state: State<'_, DesktopState>, workspace: String) -> Result<String, String> {
    let path = PathBuf::from(workspace)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    *state
        .workspace
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path.clone();
    Ok(path.display().to_string())
}

#[tauri::command]
fn configuration(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    serde_json::to_value(harness.config()).map_err(error_string)
}

#[tauri::command]
fn set_comfyui_url(
    state: State<'_, DesktopState>,
    profile: String,
    base_url: String,
) -> Result<serde_json::Value, String> {
    let config =
        genaimage_core::HarnessConfig::set_comfyui_url(&state.workspace(), &profile, &base_url)
            .map_err(error_string)?;
    serde_json::to_value(config).map_err(error_string)
}

#[tauri::command]
async fn health(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    let health = harness.health().await.map_err(error_string)?;
    serde_json::to_value(health).map_err(error_string)
}

#[tauri::command]
fn list_workflows(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    serde_json::to_value(harness.discover_workflows().map_err(error_string)?).map_err(error_string)
}

#[tauri::command]
fn inspect_workflow(
    state: State<'_, DesktopState>,
    workflow: String,
) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    let loaded = harness.load_workflow(&workflow).map_err(error_string)?;
    serde_json::to_value(serde_json::json!({
        "manifest": loaded.manifest,
        "parameters": loaded.parameter_summary(),
        "required_gates": loaded.required_gates(),
        "workflow_sha256": loaded.workflow_sha256
    }))
    .map_err(error_string)
}

#[tauri::command]
async fn submit_run(
    app: AppHandle,
    state: State<'_, DesktopState>,
    request: RunRequest,
) -> Result<RunRecord, String> {
    let harness = state.harness().map_err(error_string)?;
    let run = harness.submit(request).await.map_err(error_string)?;
    let run_id = run.id.clone();
    if run.status.is_trackable() {
        let waiting_harness = harness.clone();
        tauri::async_runtime::spawn(async move {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let waiter =
                tokio::spawn(
                    async move { waiting_harness.wait_for_run(&run_id, Some(sender)).await },
                );
            while let Some(progress) = receiver.recv().await {
                let _ = app.emit("genaimage://run-progress", progress);
            }
            if let Ok(Ok(run)) = waiter.await {
                let _ = app.emit("genaimage://run-finished", run);
            }
        });
    }
    Ok(run)
}

#[tauri::command]
fn list_runs(
    state: State<'_, DesktopState>,
    limit: Option<usize>,
) -> Result<Vec<RunRecord>, String> {
    state
        .harness()
        .map_err(error_string)?
        .list_runs(limit.unwrap_or(100))
        .map_err(error_string)
}

#[tauri::command]
fn run_details(
    state: State<'_, DesktopState>,
    run_id: String,
) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    serde_json::to_value(serde_json::json!({
        "run": harness.get_run(&run_id).map_err(error_string)?,
        "events": harness.run_events(&run_id, 200).map_err(error_string)?,
        "outputs": harness.repository().outputs(&run_id).map_err(error_string)?
    }))
    .map_err(error_string)
}

#[tauri::command]
async fn reconcile_runs(
    state: State<'_, DesktopState>,
    run_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let harness = state.harness().map_err(error_string)?;
    if let Some(run_id) = run_id {
        serde_json::to_value(harness.reconcile(&run_id).await.map_err(error_string)?)
            .map_err(error_string)
    } else {
        serde_json::to_value(harness.reconcile_all().await.map_err(error_string)?)
            .map_err(error_string)
    }
}

#[tauri::command]
async fn cancel_run(state: State<'_, DesktopState>, run_id: String) -> Result<RunRecord, String> {
    state
        .harness()
        .map_err(error_string)?
        .cancel(&run_id)
        .await
        .map_err(error_string)
}

#[tauri::command]
fn character_status(
    state: State<'_, DesktopState>,
    character_id: String,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(
        state
            .harness()
            .map_err(error_string)?
            .character_status(&character_id)
            .map_err(error_string)?,
    )
    .map_err(error_string)
}

#[tauri::command]
fn initialise_character(
    state: State<'_, DesktopState>,
    character_id: String,
    name: Option<String>,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(
        state
            .harness()
            .map_err(error_string)?
            .initialise_character(&character_id, name)
            .map_err(error_string)?,
    )
    .map_err(error_string)
}

#[tauri::command]
fn decide_gate(
    state: State<'_, DesktopState>,
    request: GateDecisionRequest,
) -> Result<serde_json::Value, String> {
    let status = if request.approved {
        ApprovalStatus::Approved
    } else {
        ApprovalStatus::Rejected
    };
    serde_json::to_value(
        state
            .harness()
            .map_err(error_string)?
            .decide_character_gate(
                &request.character_id,
                request.gate,
                status,
                request.approver,
                request.note,
            )
            .map_err(error_string)?,
    )
    .map_err(error_string)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let workspace = std::env::var_os("GENAIMAGE_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    tauri::Builder::default()
        .manage(DesktopState {
            workspace: RwLock::new(workspace),
        })
        .invoke_handler(tauri::generate_handler![
            current_workspace,
            set_workspace,
            configuration,
            set_comfyui_url,
            health,
            list_workflows,
            inspect_workflow,
            submit_run,
            list_runs,
            run_details,
            reconcile_runs,
            cancel_run,
            character_status,
            initialise_character,
            decide_gate
        ])
        .run(tauri::generate_context!())
        .expect("error while running GenAImage desktop");
}

fn error_string(error: impl ToString) -> String {
    error.to_string()
}
