use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use genaimage_core::{
    workflow::LoadedWorkflow, ApprovalStatus, GateId, Harness, HarnessError, RunRequest, RunStatus,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

fn valid_workflow() -> Value {
    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": 1,
                "steps": 20
            }
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {
                "filename_prefix": "genaimage"
            }
        }
    })
}

fn valid_manifest(workflow: &str) -> Value {
    json!({
        "schema_version": 1,
        "id": "contract-workflow",
        "version": "1.0.0",
        "title": "Contract workflow",
        "workflow": workflow,
        "generation_mode": "generic",
        "parameters": {
            "seed": {
                "node": "3",
                "input": "seed",
                "kind": "seed",
                "required": true
            },
            "steps": {
                "node": "3",
                "input": "steps",
                "kind": "integer",
                "required": true,
                "minimum": 1,
                "maximum": 100
            }
        },
        "presets": {
            "fast": {
                "seed": 42,
                "steps": 10
            }
        },
        "outputs": [
            {
                "node": "9",
                "kind": "image"
            }
        ]
    })
}

fn create_workflow(temp: &TempDir, manifest: Value, workflow: Value) -> PathBuf {
    let manifest_path = temp.path().join("workflows/test.genaimage.json");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&workflow).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    );
    manifest_path
}

#[test]
fn workflow_manifest_validates_and_applies_typed_parameters() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(&temp, valid_manifest("workflow.api.json"), valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid workflow");
    let bound = loaded
        .bind(
            Some("fast"),
            &BTreeMap::from([("seed".to_owned(), json!(99))]),
        )
        .expect("bound workflow");

    assert_eq!(bound.workflow["3"]["inputs"]["seed"], json!(99));
    assert_eq!(bound.workflow["3"]["inputs"]["steps"], json!(10));
    assert_eq!(loaded.required_gates(), Vec::new());
}

#[test]
fn workflow_rejects_ui_export_with_actionable_error() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(
        &temp,
        valid_manifest("workflow.api.json"),
        json!({ "nodes": [], "links": [] }),
    );
    let error = LoadedWorkflow::load(&path).expect_err("UI export must fail");

    assert!(
        error.to_string().contains("API format"),
        "unexpected error: {error}"
    );
}

#[test]
fn workflow_rejects_out_of_range_parameter_before_submission() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(&temp, valid_manifest("workflow.api.json"), valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid workflow");
    let error = loaded
        .bind(
            None,
            &BTreeMap::from([
                ("seed".to_owned(), json!(1)),
                ("steps".to_owned(), json!(101)),
            ]),
        )
        .expect_err("range must fail");

    assert!(error.to_string().contains("above maximum"));
}

#[test]
fn gate_approval_is_sequential_and_persistent() {
    let temp = TempDir::new().expect("temp directory");
    let character_dir = temp.path().join("characters/CHR-001");
    let mut state = genaimage_core::CharacterState::new("CHR-001", Some("Ada".to_owned()));

    let blocked = state.decide(
        GateId::Gate3,
        ApprovalStatus::Approved,
        "operator",
        "premature approval",
    );
    assert!(matches!(blocked, Err(HarnessError::Gate(_))));

    for gate in [GateId::Gate1, GateId::Gate2, GateId::Gate3] {
        state
            .decide(gate, ApprovalStatus::Approved, "operator", "approved")
            .expect("ordered approval");
    }
    state.save(&character_dir).expect("save state");

    let reloaded = genaimage_core::CharacterState::load(&character_dir).expect("reload state");
    reloaded
        .assert_approved(&[GateId::Gate3])
        .expect("Gate 3 approved");
    let markdown = fs::read_to_string(character_dir.join("production-record.md")).expect("record");
    assert!(markdown.contains("<!-- BEGIN GENAIMAGE GATE STATE -->"));
    assert!(markdown.contains("Gate 3 - Brief approval | Approved"));
    assert_eq!(reloaded.decisions.len(), 3);
}

#[test]
fn rejecting_an_earlier_gate_resets_later_approvals() {
    let mut state = genaimage_core::CharacterState::new("CHR-001", None);
    for gate in [
        GateId::Gate1,
        GateId::Gate2,
        GateId::Gate3,
        GateId::Phase04Review,
        GateId::Gate4,
        GateId::Gate5,
    ] {
        state
            .decide(gate, ApprovalStatus::Approved, "operator", "approved")
            .expect("ordered approval");
    }
    state
        .decide(
            GateId::Gate3,
            ApprovalStatus::Rejected,
            "operator",
            "brief changed",
        )
        .expect("reject earlier gate");

    assert_eq!(state.gates[&GateId::Gate3].status, ApprovalStatus::Rejected);
    assert_eq!(state.gates[&GateId::Gate5].status, ApprovalStatus::Pending);
    assert_eq!(state.decisions.len(), 7);
    assert!(
        state.assert_approved(&[GateId::Gate5]).is_err(),
        "later work must remain blocked"
    );
}

#[derive(Clone, Default)]
struct FakeComfyState {
    prompt_id: Arc<Mutex<Option<String>>>,
    complete: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    reject_prompt: Arc<AtomicBool>,
    prompt_calls: Arc<AtomicUsize>,
    upload_calls: Arc<AtomicUsize>,
    interrupt_calls: Arc<AtomicUsize>,
}

async fn fake_system() -> Json<Value> {
    Json(json!({ "system": { "os": "test" }, "devices": [] }))
}

async fn fake_object_info() -> Json<Value> {
    Json(json!({
        "KSampler": {
            "input": {
                "required": {
                    "seed": ["INT", {}],
                    "steps": ["INT", {}]
                }
            }
        },
        "SaveImage": {
            "input": {
                "required": {
                    "filename_prefix": ["STRING", {}]
                }
            }
        },
        "LoadImage": {
            "input": {
                "required": {
                    "image": ["STRING", {}]
                }
            }
        }
    }))
}

async fn fake_upload(State(state): State<FakeComfyState>, _body: Bytes) -> Json<Value> {
    state.upload_calls.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "name": "uploaded-reference.png",
        "subfolder": "",
        "type": "input"
    }))
}

async fn fake_prompt(
    State(state): State<FakeComfyState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.prompt_calls.fetch_add(1, Ordering::SeqCst);
    if state.reject_prompt.load(Ordering::SeqCst) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "deliberate fake failure" })),
        ));
    }
    let prompt_id = body["prompt_id"]
        .as_str()
        .unwrap_or("fake-prompt")
        .to_owned();
    *state.prompt_id.lock().expect("prompt lock") = Some(prompt_id.clone());
    Ok(Json(json!({
        "prompt_id": prompt_id,
        "number": 1,
        "node_errors": {}
    })))
}

async fn fake_queue(State(state): State<FakeComfyState>) -> Json<Value> {
    let prompt_id = state.prompt_id.lock().expect("prompt lock").clone();
    let has_job = !state.complete.load(Ordering::SeqCst)
        && !state.cancelled.load(Ordering::SeqCst)
        && prompt_id.is_some();
    let entry = has_job.then(|| json!([1, prompt_id, {}, {}]));
    let running = if has_job && state.running.load(Ordering::SeqCst) {
        entry.clone().into_iter().collect()
    } else {
        Vec::new()
    };
    let pending = if has_job && !state.running.load(Ordering::SeqCst) {
        entry.into_iter().collect()
    } else {
        Vec::new()
    };
    Json(json!({
        "queue_running": running,
        "queue_pending": pending
    }))
}

async fn fake_interrupt(State(state): State<FakeComfyState>) -> Json<Value> {
    state.interrupt_calls.fetch_add(1, Ordering::SeqCst);
    state.cancelled.store(true, Ordering::SeqCst);
    Json(json!({}))
}

async fn fake_queue_delete(
    State(state): State<FakeComfyState>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    state.cancelled.store(true, Ordering::SeqCst);
    Json(json!({}))
}

async fn fake_history(
    State(state): State<FakeComfyState>,
    AxumPath(prompt_id): AxumPath<String>,
) -> Result<Json<Value>, StatusCode> {
    if !state.complete.load(Ordering::SeqCst) {
        return Ok(Json(json!({})));
    }
    Ok(Json(json!({
        prompt_id: {
            "status": {
                "status_str": "success",
                "completed": true,
                "messages": []
            },
            "outputs": {
                "9": {
                    "images": [{
                        "filename": "ComfyUI_00001_.png",
                        "subfolder": "",
                        "type": "output"
                    }]
                }
            }
        }
    })))
}

async fn fake_view(Query(query): Query<BTreeMap<String, String>>) -> Result<Vec<u8>, StatusCode> {
    if !query.contains_key("filename") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(b"not-a-real-png-but-deterministic".to_vec())
}

async fn start_fake_comfyui(state: FakeComfyState) -> SocketAddr {
    let app = Router::new()
        .route("/system_stats", get(fake_system))
        .route("/object_info", get(fake_object_info))
        .route("/upload/image", post(fake_upload))
        .route("/prompt", post(fake_prompt))
        .route("/interrupt", post(fake_interrupt))
        .route("/queue", get(fake_queue).post(fake_queue_delete))
        .route("/history/{prompt_id}", get(fake_history))
        .route("/view", get(fake_view))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake ComfyUI");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake server");
    });
    address
}

#[tokio::test]
async fn fake_comfyui_run_survives_repository_reopen_and_materialises_output() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let address = start_fake_comfyui(state.clone()).await;
    write_file(
        &temp.path().join("genaimage.toml"),
        &format!(
            "[profiles.local]\nbase_url = \"http://{address}\"\nrequest_timeout_seconds = 5\nwebsocket_reconnect_seconds = 1\n"
        ),
    );
    let manifest_path = temp
        .path()
        .join("workflows/comfyui/contract.genaimage.json");
    fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("workflow dir");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&valid_workflow()).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&valid_manifest("workflow.api.json")).expect("manifest JSON"),
    );

    let harness = Harness::open(temp.path()).expect("open harness");
    let submitted = harness
        .submit(RunRequest {
            workflow: "contract-workflow".to_owned(),
            preset: Some("fast".to_owned()),
            parameters: BTreeMap::new(),
            character_id: None,
            version: Some("v0.1".to_owned()),
        })
        .await
        .expect("submit run");
    assert_eq!(submitted.status, RunStatus::Queued);

    drop(harness);
    let reopened = Harness::open(temp.path()).expect("reopen harness");
    assert_eq!(
        reopened
            .get_run(&submitted.id)
            .expect("persisted run")
            .status,
        RunStatus::Queued
    );

    state.complete.store(true, Ordering::SeqCst);
    let result = reopened.reconcile(&submitted.id).await.expect("reconcile");
    assert_eq!(result.run.status, RunStatus::Completed);
    assert_eq!(result.outputs.len(), 1);
    let manifest_path = PathBuf::from(result.run.manifest_path.expect("manifest path"));
    let manifest: Value =
        serde_json::from_slice(&fs::read(manifest_path).expect("read manifest")).expect("manifest");
    assert_eq!(manifest["run_id"], json!(submitted.id));
    assert_eq!(manifest["preset"], json!("fast"));
    assert_eq!(manifest["outputs"].as_array().expect("outputs").len(), 1);
}

#[tokio::test]
async fn gate_three_blocks_character_run_before_comfyui_submission() {
    let temp = TempDir::new().expect("temp directory");
    write_file(
        &temp.path().join("genaimage.toml"),
        "[profiles.local]\nbase_url = \"http://127.0.0.1:9\"\n",
    );
    let manifest_path = temp
        .path()
        .join("workflows/comfyui/character.genaimage.json");
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["generation_mode"] = json!("initial_concept");
    fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("workflow dir");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&valid_workflow()).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    );
    let harness = Harness::open(temp.path()).expect("open harness");
    harness
        .initialise_character("CHR-001", Some("Ada".to_owned()))
        .expect("initialise character");
    assert!(
        temp.path()
            .join("characters/CHR-001/input-package.md")
            .is_file(),
        "the character package must contain the copied input template"
    );

    let error = harness
        .submit(RunRequest {
            workflow: "character".to_owned(),
            preset: Some("fast".to_owned()),
            parameters: BTreeMap::new(),
            character_id: Some("CHR-001".to_owned()),
            version: None,
        })
        .await
        .expect_err("gate must block run");
    assert!(matches!(error, HarnessError::Gate(_)));
}

#[tokio::test]
async fn queued_prompt_is_cancelled_only_after_deletion_is_observed() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let address = start_fake_comfyui(state).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp, valid_manifest("workflow.api.json"));
    let harness = Harness::open(temp.path()).expect("open harness");
    let submitted = harness
        .submit(test_run_request("contract-workflow"))
        .await
        .expect("submit");

    let cancelling = harness.cancel(&submitted.id).await.expect("cancel");
    assert_eq!(cancelling.status, RunStatus::Cancelling);
    let reconciled = harness
        .reconcile(&submitted.id)
        .await
        .expect("reconcile cancellation");
    assert_eq!(reconciled.run.status, RunStatus::Cancelled);
}

#[tokio::test]
async fn running_prompt_is_interrupted_then_reconciled() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    state.running.store(true, Ordering::SeqCst);
    let address = start_fake_comfyui(state.clone()).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp, valid_manifest("workflow.api.json"));
    let harness = Harness::open(temp.path()).expect("open harness");
    let submitted = harness
        .submit(test_run_request("contract-workflow"))
        .await
        .expect("submit");

    let cancelling = harness.cancel(&submitted.id).await.expect("interrupt");
    assert_eq!(cancelling.status, RunStatus::Cancelling);
    assert_eq!(state.interrupt_calls.load(Ordering::SeqCst), 1);
    let reconciled = harness
        .reconcile(&submitted.id)
        .await
        .expect("reconcile interruption");
    assert_eq!(reconciled.run.status, RunStatus::Cancelled);
}

#[tokio::test]
async fn ambiguous_submission_is_persisted_without_automatic_retry() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    state.reject_prompt.store(true, Ordering::SeqCst);
    let address = start_fake_comfyui(state.clone()).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp, valid_manifest("workflow.api.json"));
    let harness = Harness::open(temp.path()).expect("open harness");

    let run = harness
        .submit(test_run_request("contract-workflow"))
        .await
        .expect("persist ambiguous submission");

    assert_eq!(run.status, RunStatus::UnknownSubmission);
    assert_eq!(state.prompt_calls.load(Ordering::SeqCst), 1);
    assert!(run.error.expect("submission error").contains("500"));
    assert_eq!(
        harness
            .repository()
            .active()
            .expect("active runs")
            .iter()
            .filter(|candidate| candidate.id == run.id)
            .count(),
        1,
        "reconcile-all must continue to see ambiguous submissions"
    );
}

#[tokio::test]
async fn image_input_is_uploaded_and_recorded_before_submission() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let address = start_fake_comfyui(state.clone()).await;
    configure_harness(&temp, address);
    let reference = temp.path().join("reference.png");
    fs::write(&reference, b"reference-payload").expect("reference");
    let manifest = json!({
        "schema_version": 1,
        "id": "image-input",
        "version": "1.0.0",
        "workflow": "workflow.api.json",
        "generation_mode": "generic",
        "parameters": {
            "reference": {
                "node": "1",
                "input": "image",
                "kind": "image_upload",
                "required": true
            }
        },
        "outputs": [{ "node": "9", "kind": "image" }]
    });
    let workflow = json!({
        "1": {
            "class_type": "LoadImage",
            "inputs": { "image": "placeholder.png" }
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": { "filename_prefix": "reference" }
        }
    });
    let manifest_path = temp
        .path()
        .join("workflows/comfyui/image-input.genaimage.json");
    fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("workflow dir");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&workflow).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    );
    let harness = Harness::open(temp.path()).expect("open harness");

    let run = harness
        .submit(RunRequest {
            workflow: "image-input".to_owned(),
            preset: None,
            parameters: BTreeMap::from([(
                "reference".to_owned(),
                json!(reference.display().to_string()),
            )]),
            character_id: None,
            version: None,
        })
        .await
        .expect("submit image workflow");

    assert_eq!(state.upload_calls.load(Ordering::SeqCst), 1);
    let resolved: Value = serde_json::from_slice(
        &fs::read(PathBuf::from(&run.output_dir).join("resolved-workflow.json"))
            .expect("resolved workflow"),
    )
    .expect("resolved JSON");
    assert_eq!(
        resolved["1"]["inputs"]["image"],
        json!("uploaded-reference.png")
    );
    let preflight: Value = serde_json::from_slice(
        &fs::read(PathBuf::from(&run.output_dir).join("preflight.json")).expect("preflight"),
    )
    .expect("preflight JSON");
    assert_eq!(
        preflight["source_inputs"][0]["parameter"],
        json!("reference")
    );
    assert_eq!(
        preflight["source_inputs"][0]["uploaded_name"],
        json!("uploaded-reference.png")
    );
}

fn configure_harness(temp: &TempDir, address: SocketAddr) {
    write_file(
        &temp.path().join("genaimage.toml"),
        &format!(
            "[profiles.local]\nbase_url = \"http://{address}\"\nrequest_timeout_seconds = 5\nwebsocket_reconnect_seconds = 1\n"
        ),
    );
}

fn create_harness_workflow(temp: &TempDir, manifest: Value) -> PathBuf {
    let manifest_path = temp
        .path()
        .join("workflows/comfyui/contract.genaimage.json");
    fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("workflow dir");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&valid_workflow()).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    );
    manifest_path
}

fn test_run_request(workflow: &str) -> RunRequest {
    RunRequest {
        workflow: workflow.to_owned(),
        preset: Some("fast".to_owned()),
        parameters: BTreeMap::new(),
        character_id: None,
        version: Some("v0.1".to_owned()),
    }
}
