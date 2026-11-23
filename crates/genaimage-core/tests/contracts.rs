use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use genaimage_core::{
    artifacts::RunManifest,
    persistence::{NewRun, RunRepository},
    workflow::{GenerationMode, LoadedWorkflow},
    RunStatus,
};
use serde_json::{json, Value};
use tempfile::TempDir;

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

fn valid_workflow() -> Value {
    serde_json::from_str(include_str!("fixtures/contract-workflow.api.json"))
        .expect("workflow fixture")
}

fn valid_manifest(workflow: &str) -> Value {
    let mut manifest: Value =
        serde_json::from_str(include_str!("fixtures/contract-workflow.genaimage.json"))
            .expect("manifest fixture");
    manifest["workflow"] = json!(workflow);
    manifest
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
fn legacy_generic_run_and_manifest_are_still_readable() {
    let temp = TempDir::new().expect("temp directory");
    let path = temp.path().join("state.sqlite3");
    let database = rusqlite::Connection::open(&path).expect("legacy database");
    database
        .execute_batch(include_str!("fixtures/legacy-runs.sql"))
        .expect("install legacy schema and run");
    drop(database);

    let repository = RunRepository::open(&path).expect("open legacy database");
    let run = repository.get("run-legacy").expect("read legacy run");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.generation_mode, GenerationMode::Generic);
    assert!(run.updated_at > run.created_at);

    let manifest: RunManifest =
        serde_json::from_str(include_str!("fixtures/legacy-run-manifest.json"))
            .expect("read legacy manifest");
    assert_eq!(manifest.run_id, run.id);
    assert_eq!(manifest.prompt_id, run.prompt_id);
    assert_eq!(manifest.workflow.id, run.workflow_id);
    assert_eq!(manifest.parameters, run.parameters);
    assert_eq!(manifest.created_at, run.created_at);
    assert_eq!(Some(manifest.completed_at), run.completed_at);
    assert_eq!(manifest.outputs[0].source_filename, "example.png");

    repository
        .insert(&NewRun {
            id: "run-new".to_owned(),
            prompt_id: "prompt-new".to_owned(),
            client_id: "client-new".to_owned(),
            workflow_id: run.workflow_id,
            workflow_version: run.workflow_version,
            workflow_sha256: run.workflow_sha256,
            generation_mode: run.generation_mode,
            status: RunStatus::Queued,
            parameters: run.parameters,
            output_dir: "runs/text-to-image/v2".to_owned(),
        })
        .expect("insert new run into legacy database");
    assert_eq!(repository.list(10).expect("list old and new runs").len(), 2);
}
