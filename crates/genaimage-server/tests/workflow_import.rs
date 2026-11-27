mod common;

use std::{fs, path::PathBuf};

use common::ApiHarness;
use genaimage_api::{
    WorkflowDetails, WorkflowImportProposal, WorkflowImportResponse, WorkflowListResponse,
};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TEXT_TO_IMAGE: &str =
    include_str!("../../../workflows/packs/core/text-to-image/api-workflow.json");
const IMG2IMG: &str = include_str!("../../genaimage-core/tests/fixtures/import/img2img.json");
const UI_FORMAT: &str = include_str!("../../genaimage-core/tests/fixtures/import/ui-format.json");
const PROBLEM_UI: &str = "urn:genaimage:problem:workflow-ui-format";

fn graph(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

async fn post(api: &ApiHarness, path: &str, body: &Value) -> (StatusCode, Value) {
    let response = api
        .request(Method::POST, path)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn propose(api: &ApiHarness, graph: &Value) -> WorkflowImportProposal {
    let (status, body) = post(api, "/api/v1/workflows/import", &json!({"graph": graph})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

fn confirm_body(graph: &Value, proposal: &WorkflowImportProposal) -> Value {
    json!({
        "graph": graph,
        "graph_sha256": proposal.graph_sha256,
        "manifest": serde_json::to_value(&proposal.manifest).unwrap(),
    })
}

async fn confirm(api: &ApiHarness, body: &Value) -> (StatusCode, Value) {
    post(api, "/api/v1/workflows/import/confirm", body).await
}

fn root(api: &ApiHarness) -> PathBuf {
    api.core.config().workflows.clone()
}

fn error_keys(problem: &Value) -> Vec<String> {
    problem["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .map(|error| error["key"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn nothing_written(api: &ApiHarness) {
    let root = root(api);
    let entries = fs::read_dir(&root)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(entries, 0, "{} should be empty", root.display());
}

#[tokio::test]
async fn proposal_is_read_only_and_valid() {
    let api = ApiHarness::start().await;
    let proposal = propose(&api, &graph(TEXT_TO_IMAGE)).await;
    assert_eq!(proposal.manifest.schema_version, 2);
    assert_eq!(proposal.manifest.workflow, "api-workflow.json");
    assert_eq!(proposal.manifest.parameters["prompt"].node, "6");
    assert_eq!(proposal.unknown_nodes.len(), 1);
    assert_eq!(proposal.unknown_nodes[0].class_type, "VAEDecode");
    assert!(proposal
        .unbound_inputs
        .iter()
        .any(|item| item.node == "3" && item.input == "model" && item.linked));
    assert_eq!(proposal.graph_sha256.len(), 64);
    assert!(!root(&api).exists(), "a proposal must not create the root");
    let again = propose(&api, &graph(TEXT_TO_IMAGE)).await;
    assert_eq!(
        serde_json::to_string(&proposal).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
    api.shutdown().await;
}

#[tokio::test]
async fn ui_exports_are_refused_with_export_instructions() {
    let api = ApiHarness::start().await;
    let (status, problem) = post(
        &api,
        "/api/v1/workflows/import",
        &json!({"graph": graph(UI_FORMAT)}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["type"], PROBLEM_UI);
    assert!(problem["recovery"]
        .as_str()
        .unwrap()
        .contains("Export (API)"));
    let manifest = propose(&api, &graph(TEXT_TO_IMAGE)).await.manifest;
    let (status, problem) = confirm(
        &api,
        &json!({"graph": graph(UI_FORMAT), "graph_sha256": "0", "manifest": manifest}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["type"], PROBLEM_UI);
    api.shutdown().await;
}

#[tokio::test]
async fn invalid_graphs_and_oversized_bodies_are_refused() {
    let api = ApiHarness::start().await;
    let (status, problem) = post(
        &api,
        "/api/v1/workflows/import",
        &json!({"graph": {"1": {"inputs": {}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        problem["type"],
        "urn:genaimage:problem:workflow-import-invalid"
    );
    assert_eq!(error_keys(&problem), ["graph.1"]);
    let (status, _) = post(&api, "/api/v1/workflows/import", &json!({"graph": [1]})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&api, "/api/v1/workflows/import", &graph(TEXT_TO_IMAGE)).await;
    assert!(status.is_client_error(), "a bare graph is not the contract");
    let huge = json!({"graph": {"1": {"class_type": "Note", "inputs": {"t": "x".repeat(2 * 1024 * 1024)}}}});
    let (status, _) = post(&api, "/api/v1/workflows/import", &huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let nodes: serde_json::Map<String, Value> = (0..1001)
        .map(|id| (id.to_string(), json!({"class_type": "Note", "inputs": {}})))
        .collect();
    let (status, problem) = post(&api, "/api/v1/workflows/import", &json!({"graph": nodes})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem["errors"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("1001"));
    api.shutdown().await;
}

#[tokio::test]
async fn confirm_creates_the_missing_root_and_the_workflow_is_discoverable() {
    let api = ApiHarness::start().await;
    let graph = graph(IMG2IMG);
    let proposal = propose(&api, &graph).await;
    assert!(!root(&api).exists());
    let (status, body) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let imported: WorkflowImportResponse = serde_json::from_value(body).unwrap();
    assert_eq!(imported.id, proposal.manifest.id);
    assert_eq!(imported.source, "user");
    let directory = root(&api).join(&imported.id);
    assert_eq!(
        PathBuf::from(&imported.manifest_path),
        directory.join("workflow.genaimage.json")
    );
    // The graph on disk is the exact normalised graph that was hashed, with exact numbers.
    let written = fs::read(directory.join("api-workflow.json")).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&written)), proposal.graph_sha256);
    let text = String::from_utf8(written).unwrap();
    assert!(text.contains("18446744073709551615"));
    assert!(text.contains("\"cfg\":2.0"));
    let manifest = fs::read_to_string(directory.join("workflow.genaimage.json")).unwrap();
    assert!(manifest.contains("\"default\": 18446744073709551615"));
    // Immediately listed and loadable, with the revision the response named.
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    let listed = list
        .workflows
        .iter()
        .find(|workflow| workflow.id == imported.id)
        .expect("listed at once");
    assert_eq!(listed.source, "user");
    assert!(listed.available);
    assert_eq!(listed.revision.as_deref(), Some(imported.revision.as_str()));
    let details: WorkflowDetails = api
        .get_json(&format!("/api/v1/workflows/{}", imported.id))
        .await;
    assert_eq!(
        details.revision.as_deref(),
        Some(imported.revision.as_str())
    );
    assert_eq!(details.parameters["image"].kind, "image_upload");
    assert!(list.diagnostics.is_empty(), "{:?}", list.diagnostics);
    assert!(api
        .core
        .load_api_workflow(&imported.id, None)
        .unwrap()
        .is_some());
    api.shutdown().await;
}

#[tokio::test]
async fn edits_to_the_manifest_are_written_as_sent() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let mut body = confirm_body(&graph, &proposal);
    body["manifest"]["id"] = json!("my-flow");
    body["manifest"]["title"] = json!("My flow");
    body["manifest"]["parameters"]
        .as_object_mut()
        .unwrap()
        .remove("denoise");
    body["manifest"]["parameters"]["steps"]["maximum"] = json!(150);
    let (status, response) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let written: Value = serde_json::from_slice(
        &fs::read(root(&api).join("my-flow/workflow.genaimage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written["title"], "My flow");
    assert!(written["parameters"].get("denoise").is_none());
    assert_eq!(written["parameters"]["steps"]["maximum"], 150);
    api.shutdown().await;
}

#[tokio::test]
async fn an_identical_confirmation_replays() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let body = confirm_body(&graph, &proposal);
    let (first_status, first) = confirm(&api, &body).await;
    let (second_status, second) = confirm(&api, &body).await;
    assert_eq!(
        (first_status, second_status),
        (StatusCode::OK, StatusCode::OK)
    );
    assert_eq!(first, second);
    let futures: Vec<_> = (0..6).map(|_| confirm(&api, &body)).collect();
    for (status, repeated) in futures_util::future::join_all(futures).await {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(repeated, first);
    }
    api.shutdown().await;
}

#[tokio::test]
async fn an_existing_directory_with_other_content_is_never_touched() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let directory = root(&api).join(&proposal.manifest.id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("api-workflow.json"), b"precious graph").unwrap();
    fs::write(directory.join("notes.txt"), b"precious notes").unwrap();
    let (status, problem) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "urn:genaimage:problem:workflow-exists");
    assert_eq!(
        fs::read(directory.join("api-workflow.json")).unwrap(),
        b"precious graph"
    );
    assert_eq!(
        fs::read(directory.join("notes.txt")).unwrap(),
        b"precious notes"
    );
    assert!(!directory.join("workflow.genaimage.json").exists());
    let suggestion = problem["errors"][0]["reason"].as_str().unwrap().to_owned();
    assert_eq!(problem["errors"][0]["key"], "suggested_id");
    assert_ne!(suggestion, proposal.manifest.id);
    let mut retry = confirm_body(&graph, &proposal);
    retry["manifest"]["id"] = json!(suggestion);
    let (status, body) = confirm(&api, &retry).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    api.shutdown().await;
}

#[tokio::test]
async fn an_id_declared_anywhere_is_refused_with_a_suggestion() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let claim = |id: &str| {
        let mut body = confirm_body(&graph, &proposal);
        body["manifest"]["id"] = json!(id);
        body
    };
    // A manifest elsewhere in the user root.
    let user = root(&api);
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("api.json"), TEXT_TO_IMAGE).unwrap();
    fs::write(
        user.join("elsewhere.genaimage.json"),
        r#"{"schema_version":2,"id":"taken-user","version":"1","workflow":"api.json"}"#,
    )
    .unwrap();
    // A pack workflow.
    let pack = api.core.config().packs[0].join("extras");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("pack.toml"),
        "id = 'extras'\nversion = '1'\nworkflows = ['wf.genaimage.json']\n",
    )
    .unwrap();
    fs::write(pack.join("api.json"), TEXT_TO_IMAGE).unwrap();
    fs::write(
        pack.join("wf.genaimage.json"),
        r#"{"schema_version":2,"id":"taken-pack","version":"1","workflow":"api.json"}"#,
    )
    .unwrap();
    // An invalid manifest still claims its id.
    fs::write(
        user.join("broken.genaimage.json"),
        r#"{"schema_version":2,"id":"taken-broken","version":"1","workflow":"missing.json"}"#,
    )
    .unwrap();
    for id in ["taken-user", "taken-pack", "taken-broken"] {
        let (status, problem) = confirm(&api, &claim(id)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{id}: {problem}");
        assert_eq!(problem["type"], "urn:genaimage:problem:workflow-exists");
        assert_eq!(problem["errors"][0]["reason"], format!("{id}-2"));
        assert!(!user.join(id).exists(), "no second claimant may be created");
    }
    api.shutdown().await;
}

#[tokio::test]
async fn a_hash_mismatch_is_rejected_and_nothing_is_written() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let mut changed = graph.clone();
    changed["3"]["inputs"]["steps"] = json!(21);
    let mut body = confirm_body(&changed, &proposal);
    let (status, problem) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        problem["type"],
        "urn:genaimage:problem:workflow-graph-mismatch"
    );
    body["graph"] = graph;
    body["graph_sha256"] = json!("00".repeat(32));
    assert_eq!(confirm(&api, &body).await.0, StatusCode::CONFLICT);
    nothing_written(&api);
    api.shutdown().await;
}

#[tokio::test]
async fn an_invalid_manifest_is_rejected_with_its_errors_and_never_coerced() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let mut body = confirm_body(&graph, &proposal);
    body["manifest"]["parameters"]["steps"]["default"] = json!("twenty");
    body["manifest"]["parameters"]["cfg"]["node"] = json!("404");
    body["manifest"]["outputs"] = json!([{"node": "999"}]);
    body["manifest"]["workflow"] = json!("../escape.json");
    let (status, problem) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        problem["type"],
        "urn:genaimage:problem:workflow-manifest-invalid"
    );
    assert_eq!(error_keys(&problem), ["workflow"]);
    body["manifest"]["workflow"] = json!("api-workflow.json");
    let (status, problem) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let keys = error_keys(&problem);
    assert!(keys.contains(&"parameters.steps".to_owned()), "{keys:?}");
    assert!(keys.contains(&"parameters.cfg".to_owned()), "{keys:?}");
    assert!(keys.contains(&"outputs".to_owned()), "{keys:?}");
    let mut unknown = confirm_body(&graph, &proposal);
    unknown["manifest"]["surprise"] = json!(true);
    assert!(confirm(&api, &unknown).await.0.is_client_error());
    nothing_written(&api);
    api.shutdown().await;
}

#[tokio::test]
async fn reserved_and_unsafe_ids_are_rejected() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    for id in [
        "refresh", "import", ".", "..", "../evil", "a/b", "A", "", "-x", "a b",
    ] {
        let mut body = confirm_body(&graph, &proposal);
        body["manifest"]["id"] = json!(id);
        let (status, problem) = confirm(&api, &body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{id:?}: {problem}"
        );
        assert_eq!(error_keys(&problem)[0], "id", "{id:?}");
    }
    nothing_written(&api);
    assert!(!api.workspace().join("evil").exists());
    api.shutdown().await;
}

#[tokio::test]
async fn discovery_treats_import_as_reserved() {
    let api = ApiHarness::start().await;
    let user = root(&api);
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("api.json"), TEXT_TO_IMAGE).unwrap();
    fs::write(
        user.join("import.genaimage.json"),
        r#"{"schema_version":2,"id":"import","version":"1","workflow":"api.json"}"#,
    )
    .unwrap();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert!(list.workflows.is_empty());
    assert!(list.diagnostics[0].error.contains("reserved"));
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_in_place_of_the_directory_is_not_followed() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let outside = api.workspace().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(root(&api)).unwrap();
    std::os::unix::fs::symlink(&outside, root(&api).join(&proposal.manifest.id)).unwrap();
    let (status, _) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn an_unwritable_root_is_an_error_not_a_success() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    fs::write(root(&api), b"a file where the root should be").unwrap();
    let (status, _) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert!(status.is_server_error(), "{status}");
    api.shutdown().await;
}
