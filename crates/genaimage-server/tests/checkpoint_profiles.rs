mod common;

use std::fs;

use common::ApiHarness;
use genaimage_api::{
    CheckpointsResponse, RunListResponse, RunResponse, WorkflowDetails, WorkflowValidationResponse,
    WorkflowValueSource,
};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

const PROFILES: &str = r#"
[[profiles]]
id = "base"
label = "Base"
checkpoints = ["b.safetensors"]
note = "Base settings."
[profiles.values]
steps = 28
cfg = 2.0
unrelated = "ignored by workflows that lack it"

[[profiles]]
id = "base-and-c"
label = "Base and C"
checkpoints = ["b.safetensors", "c.ckpt"]
"#;

fn install_pack(api: &ApiHarness, profiles: &str) {
    let dir = api.core.config().packs[0].join("chars");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("pack.toml"),
        "id = 'chars'\nversion = '1'\nworkflows = ['wf.genaimage.json']\ncheckpoint_profiles = 'checkpoints.toml'\n",
    )
    .unwrap();
    fs::write(
        dir.join("workflow.api.json"),
        json!({
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7.5}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "a.ckpt"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "g"}}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("wf.genaimage.json"),
        json!({
            "schema_version": 2, "id": "chars-flow", "version": "1", "workflow": "workflow.api.json",
            "parameters": {
                "seed": {"node": "3", "input": "seed", "kind": "seed", "default": 1},
                "steps": {"node": "3", "input": "steps", "kind": "integer", "default": 20, "minimum": 1, "maximum": 100},
                "cfg": {"node": "3", "input": "cfg", "kind": "number", "default": 7.5},
                "checkpoint": {"node": "4", "input": "ckpt_name", "kind": "string", "default": "a.ckpt"}
            },
            "presets": {"portrait": {"steps": 30}},
            "preset_info": {"portrait": {"label": "Portrait 2:3", "kind": "layout"}},
            "outputs": [{"node": "9", "kind": "image"}]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(dir.join("checkpoints.toml"), profiles).unwrap();
}

fn ids(response: &CheckpointsResponse, name: &str) -> Vec<String> {
    response.profiles[name]
        .iter()
        .map(|profile| format!("{}/{}", profile.pack, profile.id))
        .collect()
}

#[tokio::test]
async fn the_checkpoints_listing_names_profiles_that_match_exactly() {
    let api = ApiHarness::start().await;
    install_pack(&api, PROFILES);
    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [[
            "a.ckpt", "B.safetensors", "b.safetensors", "c.ckpt"
        ], {}]}}}
    }));
    let response: CheckpointsResponse = api.get_json("/api/v1/models/checkpoints").await;
    assert_eq!(
        ids(&response, "b.safetensors"),
        ["chars/base", "chars/base-and-c"]
    );
    assert_eq!(ids(&response, "c.ckpt"), ["chars/base-and-c"]);
    assert!(ids(&response, "B.safetensors").is_empty(), "case differs");
    assert!(ids(&response, "a.ckpt").is_empty());
    assert_eq!(response.profiles.len(), response.checkpoints.len());
    assert!(response
        .profile_note
        .contains("no model family is inferred"));
}

#[tokio::test]
async fn a_malformed_profile_file_contributes_no_matches_and_is_a_pack_problem() {
    let api = ApiHarness::start().await;
    install_pack(
        &api,
        "[[profiles]]\nid = 'x'\nlabel = 'X'\ncheckpoints = []\n",
    );
    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["b.safetensors"], {}]}}}
    }));
    let response: CheckpointsResponse = api.get_json("/api/v1/models/checkpoints").await;
    assert!(ids(&response, "b.safetensors").is_empty());
    let packs: genaimage_api::PackListResponse = api.get_json("/api/v1/packs").await;
    let diagnostics = &packs.packs[0].diagnostics;
    assert!(
        diagnostics
            .iter()
            .any(|item| item.contains("exact checkpoint names")),
        "{diagnostics:?}"
    );
}

#[tokio::test]
async fn details_validation_and_submission_use_the_named_profile_only() {
    let api = ApiHarness::start().await;
    install_pack(&api, PROFILES);
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/chars-flow").await;
    assert_eq!(details.checkpoint_parameter.as_deref(), Some("checkpoint"));
    assert_eq!(
        details.preset_info["portrait"].label.as_deref(),
        Some("Portrait 2:3")
    );
    let base = &details.checkpoint_profiles[0];
    assert_eq!(base.id, "base");
    assert_eq!(base.note.as_deref(), Some("Base settings."));
    assert_eq!(
        base.values.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cfg", "steps"],
        "values for parameters the workflow lacks are not applied to it"
    );
    assert_eq!(base.values["cfg"].to_string(), "2.0");

    let path = "/api/v1/workflows/chars-flow/validate";
    let body = json!({
        "preset": "portrait", "checkpoint_profile": "base",
        "parameters": {"checkpoint": "b.safetensors"}
    });
    let validation: WorkflowValidationResponse = api.post_json(path, &body).await;
    let source = |name: &str| {
        let value = validation
            .effective_values
            .iter()
            .find(|value| value.parameter == name)
            .unwrap();
        (
            value.value.to_string(),
            value.source,
            value.checkpoint_profile.clone(),
        )
    };
    assert_eq!(
        source("steps"),
        ("30".into(), WorkflowValueSource::Preset, None)
    );
    assert_eq!(
        source("cfg"),
        (
            "2.0".into(),
            WorkflowValueSource::CheckpointProfile,
            Some("base".into())
        )
    );
    assert_eq!(
        source("seed"),
        ("1".into(), WorkflowValueSource::Default, None)
    );
    assert_eq!(
        source("checkpoint"),
        ("\"b.safetensors\"".into(), WorkflowValueSource::User, None)
    );

    let unnamed: WorkflowValidationResponse = api
        .post_json(
            path,
            &json!({"parameters": {"checkpoint": "b.safetensors"}}),
        )
        .await;
    let cfg = unnamed
        .effective_values
        .iter()
        .find(|v| v.parameter == "cfg")
        .unwrap();
    assert_eq!(cfg.value.to_string(), "7.5");
    assert_eq!(cfg.source, WorkflowValueSource::Default);

    let mismatch = api
        .request(Method::POST, path)
        .json(&json!({"checkpoint_profile": "base"}))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(mismatch, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert!(
        problem.detail.contains("matching is exact"),
        "{}",
        problem.detail
    );
}

#[tokio::test]
async fn a_run_with_a_profile_is_pinned_idempotent_and_a_mismatch_creates_nothing() {
    let api = ApiHarness::start().await;
    install_pack(&api, PROFILES);
    let submit = |key: &'static str, body: Value| {
        let request = api
            .request(Method::POST, "/api/v1/runs")
            .header("Idempotency-Key", key)
            .json(&body);
        async move { request.send().await.unwrap() }
    };
    let rejected = submit(
        "k0",
        json!({"workflow": "chars-flow", "checkpoint_profile": "base"}),
    )
    .await;
    ApiHarness::assert_problem(rejected, StatusCode::UNPROCESSABLE_ENTITY).await;
    let runs: RunListResponse = api.get_json("/api/v1/runs").await;
    assert!(runs.runs.is_empty());

    let body = json!({
        "workflow": "chars-flow", "checkpoint_profile": "base",
        "parameters": {"checkpoint": "b.safetensors"}
    });
    let created = submit("k1", body.clone()).await;
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let run: RunResponse = created.json().await.unwrap();
    assert_eq!(run.parameters["steps"].to_string(), "28");
    assert_eq!(run.parameters["cfg"].to_string(), "2.0");
    let replay = submit("k1", body.clone()).await;
    assert_eq!(replay.status(), StatusCode::OK);
    let without = json!({"workflow": "chars-flow", "parameters": {"checkpoint": "b.safetensors"}});
    let conflict = submit("k1", without).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}
