use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use axum::{routing::get, Json, Router};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    config::ComfyUiProfile, Harness, HarnessConfig, ImageBackend, RunRequest, RunStatus,
};
use genaimage_test_support::{FakeComfyServer, FakeComfyState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

#[tokio::test]
async fn fake_comfyui_run_survives_repository_reopen_and_materialises_output() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let fake = FakeComfyServer::start(state.clone()).await;
    let address = fake.address();
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

    let harness = open_harness(temp.path()).await.expect("open harness");
    let submitted = harness
        .submit(RunRequest {
            workflow_revision: None,
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
    let reopened = open_harness(temp.path()).await.expect("reopen harness");
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
async fn queue_deletion_acknowledgement_without_history_is_not_confirmation() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    let fake = FakeComfyServer::start(state).await;
    let address = fake.address();
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).await.expect("open harness");
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
    assert_eq!(reconciled.run.status, RunStatus::Cancelling);
}

#[tokio::test]
async fn running_prompt_is_interrupted_then_reconciled() {
    let temp = TempDir::new().expect("temp directory");
    let state = FakeComfyState::default();
    state.running.store(true, Ordering::SeqCst);
    let fake = FakeComfyServer::start(state.clone()).await;
    let address = fake.address();
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).await.expect("open harness");
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
    let fake = FakeComfyServer::start(state.clone()).await;
    let address = fake.address();
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).await.expect("open harness");

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
    let fake = FakeComfyServer::start(state.clone()).await;
    let address = fake.address();
    configure_harness(&temp, address);
    create_harness_workflow(&temp);
    let harness = open_harness(temp.path()).await.expect("open harness");
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
    let fake = FakeComfyServer::start(state.clone()).await;
    let address = fake.address();
    configure_harness(&temp, address);
    let reference = temp.path().join("reference.png");
    fs::write(&reference, b"reference-payload").expect("reference");
    let manifest = json!({
        "schema_version": 2,
        "id": "image-input",
        "version": "1.0.0",
        "workflow": "workflow.api.json",
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
    let harness = open_harness(temp.path()).await.expect("open harness");

    let run = harness
        .submit(RunRequest {
            workflow_revision: None,
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

async fn open_harness(workspace: &Path) -> genaimage_core::Result<Harness> {
    let config_path = workspace.join("genaimage.toml");
    let config =
        HarnessConfig::load_with(workspace, Some(&config_path), workspace, |key| match key {
            "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => Some(workspace.display().to_string()),
            _ => None,
        })?;
    let backend = Arc::new(ComfyUiClient::new(&config.comfyui)?);
    Harness::new_async(config, backend).await
}

fn configure_harness(temp: &TempDir, address: SocketAddr) {
    write_file(
        &temp.path().join("genaimage.toml"),
        &format!(
            "[comfyui]\nbase_url = \"http://{address}\"\nrequest_timeout_seconds = 5\nwebsocket_reconnect_seconds = 1\n[paths]\nstate_database = \"state.sqlite3\"\nruns = \"runs\"\n"
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
        workflow_revision: None,
        workflow: workflow.to_owned(),
        preset: Some("fast".to_owned()),
        parameters: BTreeMap::new(),
        version: Some("v0.1".to_owned()),
    }
}

#[tokio::test]
async fn http_redirects_are_not_followed() {
    let target_hits = Arc::new(AtomicUsize::new(0));
    let hits = target_hits.clone();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            target,
            Router::new().route(
                "/system_stats",
                get(move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(json!({}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_addr = source.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            source,
            Router::new().route(
                "/system_stats",
                get(move || async move {
                    axum::response::Redirect::temporary(&format!(
                        "http://{target_addr}/system_stats"
                    ))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let profile = ComfyUiProfile {
        base_url: format!("http://{source_addr}"),
        ..Default::default()
    };
    assert!(ComfyUiClient::new(&profile)
        .unwrap()
        .health()
        .await
        .is_err());
    assert_eq!(target_hits.load(Ordering::SeqCst), 0);
}
