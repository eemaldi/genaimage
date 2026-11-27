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

fn hex_sha(graph: &Value) -> String {
    hex::encode(Sha256::digest(canonical_bytes(graph)))
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
    assert!(problem.get("errors").is_none(), "{problem}");
    let suggestion = problem["suggested_id"].as_str().unwrap().to_owned();
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
        assert_eq!(problem["suggested_id"], format!("{id}-2"));
        assert!(problem.get("errors").is_none(), "{problem}");
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
    assert_eq!(problem["expected_graph_sha256"], hex_sha(&changed));
    assert!(problem.get("errors").is_none(), "{problem}");
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

fn claim_in_a_pack(api: &ApiHarness, id: &str) {
    let pack = api.core.config().packs[0].join("late-claimant");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("pack.toml"),
        "id = 'late-claimant'\nversion = '1'\nworkflows = ['wf.genaimage.json']\n",
    )
    .unwrap();
    fs::write(pack.join("api.json"), TEXT_TO_IMAGE).unwrap();
    fs::write(
        pack.join("wf.genaimage.json"),
        format!(r#"{{"schema_version":2,"id":"{id}","version":"1","workflow":"api.json"}}"#),
    )
    .unwrap();
}

#[tokio::test]
async fn a_duplicate_claimant_after_the_write_says_the_workflow_was_written() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let body = confirm_body(&graph, &proposal);
    let (status, first) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    claim_in_a_pack(&api, &proposal.manifest.id);
    // The retry of a confirmation whose outcome the client could not see.
    let (status, problem) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["type"], "urn:genaimage:problem:workflow-ambiguous");
    assert_eq!(problem["id"], proposal.manifest.id.as_str());
    assert_eq!(problem["manifest_path"], first["manifest_path"]);
    let detail = problem["detail"].as_str().unwrap();
    assert!(
        detail.contains(&format!(
            "was written at '{}'",
            first["manifest_path"].as_str().unwrap()
        )),
        "{detail}"
    );
    assert!(
        detail.contains("unavailable until the duplicate is removed or renamed"),
        "{detail}"
    );
    assert!(problem["recovery"]
        .as_str()
        .unwrap()
        .contains("Do not confirm"));
    assert!(problem.get("errors").is_none());
    // The write is kept, and the retry created no orphan.
    let directories: Vec<_> = fs::read_dir(root(&api))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "packs")
        .collect();
    assert_eq!(directories, std::slice::from_ref(&proposal.manifest.id));
    assert!(root(&api)
        .join(&proposal.manifest.id)
        .join("workflow.genaimage.json")
        .exists());
    api.shutdown().await;
}

#[tokio::test]
async fn a_crash_leftover_with_only_the_identical_graph_is_completed() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let body = confirm_body(&graph, &proposal);
    let directory = root(&api).join(&proposal.manifest.id);
    fs::create_dir_all(&directory).unwrap();
    let canonical = canonical_bytes(&graph);
    fs::write(directory.join("api-workflow.json"), &canonical).unwrap();
    let stale = directory.join(format!(
        ".workflow.genaimage.json.{}.tmp",
        "0a1b2c3d-0000-4000-8000-000000000001"
    ));
    fs::write(&stale, b"half a manifest").unwrap();
    let futures: Vec<_> = (0..4).map(|_| confirm(&api, &body)).collect();
    let results = futures_util::future::join_all(futures).await;
    for (status, response) in &results {
        assert_eq!(*status, StatusCode::OK, "{response}");
        assert_eq!(response, &results[0].1);
    }
    assert!(directory.join("workflow.genaimage.json").exists());
    assert_eq!(
        fs::read(&stale).unwrap(),
        b"half a manifest",
        "foreign files are never touched"
    );
    assert_eq!(
        fs::read(directory.join("api-workflow.json")).unwrap(),
        canonical
    );
    let details: WorkflowDetails = api
        .get_json(&format!("/api/v1/workflows/{}", proposal.manifest.id))
        .await;
    assert_eq!(
        details.revision.as_deref(),
        results[0].1["revision"].as_str()
    );
    api.shutdown().await;
}

#[tokio::test]
async fn a_leftover_directory_with_anything_else_stays_exists() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let directory = root(&api).join(&proposal.manifest.id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("api-workflow.json"), canonical_bytes(&graph)).unwrap();
    fs::write(directory.join("thumbnail.png"), b"user file").unwrap();
    let (status, problem) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "urn:genaimage:problem:workflow-exists");
    assert!(!directory.join("workflow.genaimage.json").exists());
    assert_eq!(
        fs::read(directory.join("thumbnail.png")).unwrap(),
        b"user file"
    );
    api.shutdown().await;
}

fn canonical_bytes(graph: &Value) -> Vec<u8> {
    genaimage_core::workflow_import::canonical_graph_bytes(graph)
}

#[tokio::test]
async fn concurrent_confirmations_with_different_manifests_have_one_winner() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let bodies: Vec<Value> = ["Alpha", "Beta", "Gamma", "Delta"]
        .iter()
        .map(|title| {
            let mut body = confirm_body(&graph, &proposal);
            body["manifest"]["title"] = json!(title);
            body
        })
        .collect();
    let results =
        futures_util::future::join_all(bodies.iter().map(|body| confirm(&api, body))).await;
    let winners: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, (status, _))| *status == StatusCode::OK)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    for (index, (status, problem)) in results.iter().enumerate() {
        if index != winners[0] {
            assert_eq!(*status, StatusCode::CONFLICT, "{problem}");
            assert_eq!(problem["type"], "urn:genaimage:problem:workflow-exists");
        }
    }
    let directory = root(&api).join(&proposal.manifest.id);
    let on_disk: Value =
        serde_json::from_slice(&fs::read(directory.join("workflow.genaimage.json")).unwrap())
            .unwrap();
    let title = ["Alpha", "Beta", "Gamma", "Delta"][winners[0]];
    assert_eq!(on_disk["title"], title);
    let written = fs::read_to_string(directory.join("workflow.genaimage.json")).unwrap();
    assert!(
        written.contains(&format!("\"title\": \"{title}\"")),
        "{written}"
    );
    let names: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    api.shutdown().await;
}

#[tokio::test]
async fn exact_numbers_survive_confirmation_and_inspection() {
    let api = ApiHarness::start().await;
    let source = r#"{
      "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "m.safetensors"}},
      "2": {"class_type": "LoraLoader", "inputs": {"model": ["1", 0], "clip": ["1", 1], "lora_name": "l.safetensors", "strength_model": -0.0, "strength_clip": 1.0}},
      "3": {"class_type": "KSampler", "inputs": {"seed": 18446744073709551615, "steps": 20, "cfg": 2.0, "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0, "model": ["2", 0], "positive": ["4", 0], "negative": ["4", 0], "latent_image": ["5", 0]}},
      "4": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["2", 1]}},
      "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 512, "height": 512, "batch_size": 1}},
      "6": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "p"}}
    }"#;
    let graph = graph(source);
    let proposal = propose(&api, &graph).await;
    let (status, response) = confirm(&api, &confirm_body(&graph, &proposal)).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let id = response["id"].as_str().unwrap();
    let directory = root(&api).join(id);
    let written = fs::read_to_string(directory.join("api-workflow.json")).unwrap();
    for literal in [
        r#""seed":18446744073709551615"#,
        r#""cfg":2.0"#,
        r#""strength_model":-0.0"#,
        r#""strength_clip":1.0"#,
    ] {
        assert!(written.contains(literal), "{literal} in {written}");
    }
    let manifest = fs::read_to_string(directory.join("workflow.genaimage.json")).unwrap();
    for literal in [
        r#""default": 18446744073709551615"#,
        r#""default": 2.0"#,
        r#""default": -0.0"#,
    ] {
        assert!(manifest.contains(literal), "{literal} in {manifest}");
    }
    let details: Value = api.get_json(&format!("/api/v1/workflows/{id}")).await;
    let defaults: Vec<String> = ["seed", "cfg", "lora_strength_model"]
        .iter()
        .map(|name| details["parameters"][name]["default"].to_string())
        .collect();
    assert_eq!(
        defaults,
        ["18446744073709551615", "2.0", "-0.0"],
        "{details}"
    );
    api.shutdown().await;
}

#[tokio::test]
async fn an_explicit_null_default_is_a_manifest_error() {
    let api = ApiHarness::start().await;
    let graph = graph(TEXT_TO_IMAGE);
    let proposal = propose(&api, &graph).await;
    let mut body = confirm_body(&graph, &proposal);
    body["manifest"]["parameters"]["steps"]["default"] = Value::Null;
    let (status, problem) = confirm(&api, &body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(
        problem["type"],
        "urn:genaimage:problem:workflow-manifest-invalid"
    );
    assert_eq!(error_keys(&problem), ["parameters.steps"]);
    nothing_written(&api);
    api.shutdown().await;
}
