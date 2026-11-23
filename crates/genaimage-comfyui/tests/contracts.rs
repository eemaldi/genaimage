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
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{Harness, HarnessConfig, RunRequest, RunStatus};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

#[derive(Clone, Default)]
struct FakeComfyState {
    prompt_id: Arc<Mutex<Option<String>>>,
    complete: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    fail_submit_response: Arc<AtomicBool>,
    redirect_submit_response: Arc<AtomicBool>,
    prompt_calls: Arc<AtomicUsize>,
    upload_calls: Arc<AtomicUsize>,
    interrupt_calls: Arc<AtomicUsize>,
}

async fn fake_system() -> Json<Value> {
    Json(json!({ "system": { "os": "test" }, "devices": [] }))
}

async fn fake_object_info() -> Json<Value> {
    Json(json!({
        "CheckpointLoaderSimple": {},
        "CLIPTextEncode": {},
        "EmptyLatentImage": {},
        "VAEDecode": {},
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

async fn fake_prompt(State(state): State<FakeComfyState>, Json(body): Json<Value>) -> Response {
    state.prompt_calls.fetch_add(1, Ordering::SeqCst);
    let prompt_id = body["prompt_id"].as_str().expect("prompt ID").to_owned();
    *state.prompt_id.lock().expect("prompt lock") = Some(prompt_id.clone());
    if state.redirect_submit_response.load(Ordering::SeqCst) {
        return Redirect::temporary("/prompt").into_response();
    }
    if state.fail_submit_response.load(Ordering::SeqCst) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "deliberate fake failure" })),
        )
            .into_response();
    }
    Json(json!({
        "prompt_id": prompt_id,
        "number": 1,
        "node_errors": {}
    }))
    .into_response()
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
    configure_harness(&temp, address);
    let workflow_dir = temp.path().join("workflows/comfyui/text-to-image");
    write_file(
        &workflow_dir.join("workflow.genaimage.json"),
        include_str!("../../../workflows/comfyui/text-to-image/workflow.genaimage.json"),
    );
    write_file(
        &workflow_dir.join("api-workflow.json"),
        include_str!("../../../workflows/comfyui/text-to-image/api-workflow.json"),
    );

    let harness = open_harness(temp.path()).expect("open harness");
    let submitted = harness
        .submit(RunRequest {
            workflow: "text-to-image".to_owned(),
            preset: Some("fast".to_owned()),
            parameters: BTreeMap::from([
                ("checkpoint".to_owned(), json!("explicit.safetensors")),
                ("positive_prompt".to_owned(), json!("A blue sphere")),
                ("seed".to_owned(), json!(99)),
            ]),
            version: Some("v0.1".to_owned()),
        })
        .await
        .expect("submit run");
    assert_eq!(submitted.status, RunStatus::Queued);

    drop(harness);
    let reopened = open_harness(temp.path()).expect("reopen harness");
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
    assert_eq!(
        manifest["parameters"]["checkpoint"],
        json!("explicit.safetensors")
    );
    assert_eq!(manifest["parameters"]["seed"], json!(99));
    assert_eq!(manifest["outputs"].as_array().expect("outputs").len(), 1);
}

#[tokio::test]
async fn queued_prompt_is_cancelled_only_after_deletion_is_observed() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let address = start_fake_comfyui(state).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).expect("open harness");
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
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).expect("open harness");
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
async fn ambiguous_submission_is_reconciled_without_automatic_retry() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    state.fail_submit_response.store(true, Ordering::SeqCst);
    let address = start_fake_comfyui(state.clone()).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).expect("open harness");

    let run = harness
        .submit(test_run_request("contract-workflow"))
        .await
        .expect("persist ambiguous submission");

    assert_eq!(run.status, RunStatus::UnknownSubmission);
    assert_eq!(state.prompt_calls.load(Ordering::SeqCst), 1);
    assert!(run.error.expect("submission error").contains("500"));
    let reconciled = harness
        .reconcile_all()
        .await
        .expect("reconcile active runs");
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].run.id, run.id);
    assert_eq!(reconciled[0].run.status, RunStatus::Queued);
    assert_eq!(state.prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn submission_is_not_replayed_after_a_post_preserving_redirect() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    state.redirect_submit_response.store(true, Ordering::SeqCst);
    let address = start_fake_comfyui(state.clone()).await;
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).expect("open harness");
    let run = harness
        .submit(test_run_request("contract-workflow"))
        .await
        .expect("submit run");
    assert_eq!(run.status, RunStatus::UnknownSubmission);
    assert_eq!(state.prompt_calls.load(Ordering::SeqCst), 1);
    let reconciled = harness
        .reconcile(&run.id)
        .await
        .expect("reconcile redirected response");
    assert_eq!(reconciled.run.status, RunStatus::Queued);
    assert_eq!(state.prompt_calls.load(Ordering::SeqCst), 1);
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
    let harness = open_harness(temp.path()).expect("open harness");

    let run = harness
        .submit(RunRequest {
            workflow: "image-input".to_owned(),
            preset: None,
            parameters: BTreeMap::from([(
                "reference".to_owned(),
                json!(reference.display().to_string()),
            )]),
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

fn open_harness(workspace: &Path) -> genaimage_core::Result<Harness> {
    let config = HarnessConfig::load(workspace)?;
    let backend = Arc::new(ComfyUiClient::new(&config.comfyui)?);
    Harness::new(config, backend)
}

fn configure_harness(temp: &TempDir, address: SocketAddr) {
    write_file(
        &temp.path().join("genaimage.toml"),
        &format!(
            "[profiles.local]\nbase_url = \"http://{address}\"\nrequest_timeout_seconds = 5\nwebsocket_reconnect_seconds = 1\n"
        ),
    );
}

fn create_harness_workflow(temp: &TempDir) {
    let workflows = temp.path().join("workflows/comfyui");
    write_file(
        &workflows.join("workflow.api.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
    );
    write_file(
        &workflows.join("contract.genaimage.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"),
    );
}

fn test_run_request(workflow: &str) -> RunRequest {
    RunRequest {
        workflow: workflow.to_owned(),
        preset: Some("fast".to_owned()),
        parameters: BTreeMap::new(),
        version: Some("v0.1".to_owned()),
    }
}
