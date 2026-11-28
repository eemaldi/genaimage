mod common;

use std::fs;

use common::ApiHarness;
use genaimage_api::{
    PackListResponse, PackRequirementStatus, PackValidationResponse, PackValidationStatus,
    RunResponse, WorkflowDetails, WorkflowListResponse, WorkflowValidationResponse,
    WorkflowValueSource,
};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

const PACK: &str = "character-art";
const WORKFLOW_IDS: [&str; 4] = ["portrait", "sketch-to-image", "inpaint", "turnaround"];
const SDXL: &str = "sd_xl_base_1.0.safetensors";
const SD15: &str = "v1-5-pruned-emaonly.safetensors";
const CLASSES: [&str; 10] = [
    "CheckpointLoaderSimple",
    "CLIPTextEncode",
    "EmptyLatentImage",
    "ImageToMask",
    "KSampler",
    "LoadImage",
    "SaveImage",
    "VAEDecode",
    "VAEEncode",
    "VAEEncodeForInpaint",
];

async fn start() -> ApiHarness {
    let root = fs::canonicalize(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../workflows/packs"
    ))
    .unwrap()
    .display()
    .to_string();
    let api = ApiHarness::start_with_config(5, &[("GENAIMAGE_PACKS", root.as_str())]).await;
    let object_info: serde_json::Map<String, Value> = CLASSES
        .iter()
        .map(|class| ((*class).to_owned(), json!({})))
        .collect();
    *api.fake.object_info_override.lock().unwrap() = Some(Value::Object(object_info));
    api
}

fn effective(validation: &WorkflowValidationResponse, name: &str) -> (String, WorkflowValueSource) {
    let value = validation
        .effective_values
        .iter()
        .find(|value| value.parameter == name)
        .unwrap_or_else(|| panic!("no effective value for {name}"));
    (value.value.to_string(), value.source)
}

#[tokio::test]
async fn the_pack_loads_without_problems_and_its_requirements_are_met() {
    let api = start().await;
    let list: PackListResponse = api.get_json("/api/v1/packs").await;
    assert!(
        list.diagnostics
            .iter()
            .all(|item| !item.path.contains(PACK)),
        "{:?}",
        list.diagnostics
    );
    let pack = list.packs.iter().find(|pack| pack.id == PACK).unwrap();
    assert!(pack.diagnostics.is_empty(), "{:?}", pack.diagnostics);
    let mut ids = pack.workflow_ids.clone();
    ids.sort();
    let mut expected = WORKFLOW_IDS.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(
        pack.templates,
        ["templates/concept-art.toml", "templates/turnaround.toml"]
    );

    let validation: PackValidationResponse = api
        .get_json(&format!("/api/v1/packs/{PACK}/validate"))
        .await;
    assert_eq!(validation.status, PackValidationStatus::Ok);
    assert!(
        validation.diagnostics.is_empty(),
        "{:?}",
        validation.diagnostics
    );
    assert_eq!(validation.node_classes.len(), CLASSES.len());
    assert!(validation
        .node_classes
        .iter()
        .all(|check| check.status == PackRequirementStatus::Available));
    assert!(validation.models.is_empty(), "checkpoints are user-chosen");

    let workflows: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    for id in WORKFLOW_IDS {
        let workflow = workflows.workflows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(workflow.source, format!("pack:{PACK}"));
        assert!(workflow.available, "{id}");
    }
    api.shutdown().await;
}

#[tokio::test]
async fn each_workflow_declares_its_parameters_layouts_and_both_profiles() {
    let api = start().await;
    for id in WORKFLOW_IDS {
        let details: WorkflowDetails = api.get_json(&format!("/api/v1/workflows/{id}")).await;
        assert_eq!(
            details.checkpoint_parameter.as_deref(),
            Some("checkpoint"),
            "{id}"
        );
        assert!(details.parameters["checkpoint"].required, "{id}");
        assert!(details.parameters["checkpoint"].default.is_none(), "{id}");
        for name in [
            "steps",
            "cfg",
            "sampler_name",
            "scheduler",
            "seed",
            "filename_prefix",
        ] {
            assert_eq!(details.parameters[name].advanced, Some(true), "{id}.{name}");
        }
        assert_eq!(details.parameters["prompt"].advanced, None, "{id}");
        let profiles: Vec<_> = details
            .checkpoint_profiles
            .iter()
            .map(|profile| (profile.id.as_str(), profile.ignored.is_empty()))
            .collect();
        assert_eq!(profiles, [("sdxl-base", true), ("sd15", true)], "{id}");
    }
    let portrait: WorkflowDetails = api.get_json("/api/v1/workflows/portrait").await;
    assert_eq!(
        portrait.preset_info["portrait-2x3"].label.as_deref(),
        Some("Portrait 2:3")
    );
    assert_eq!(portrait.presets.len(), 3);
    let sketch: WorkflowDetails = api.get_json("/api/v1/workflows/sketch-to-image").await;
    assert!(sketch.presets.is_empty());
    assert_eq!(sketch.parameters["denoise"].default, Some(json!(0.6)));
    api.shutdown().await;
}

#[tokio::test]
async fn a_checkpoint_profile_applies_its_exact_values_and_reports_nothing_ignored() {
    let api = start().await;
    let path = "/api/v1/workflows/portrait/validate";
    let sdxl: WorkflowValidationResponse = api
        .post_json(
            path,
            &json!({
                "preset": "portrait-3x4", "checkpoint_profile": "sdxl-base",
                "parameters": {"checkpoint": SDXL, "prompt": "a figure"}
            }),
        )
        .await;
    assert!(sdxl.valid, "{:?}", sdxl.parameter_errors);
    assert!(sdxl.ignored_profile_values.is_empty());
    assert_eq!(
        effective(&sdxl, "steps"),
        ("30".into(), WorkflowValueSource::CheckpointProfile)
    );
    assert_eq!(
        effective(&sdxl, "cfg"),
        ("7.0".into(), WorkflowValueSource::CheckpointProfile)
    );
    assert_eq!(
        effective(&sdxl, "width"),
        ("896".into(), WorkflowValueSource::Preset)
    );
    assert_eq!(
        effective(&sdxl, "seed"),
        ("42".into(), WorkflowValueSource::Default)
    );

    let sd15: WorkflowValidationResponse = api
        .post_json(
            path,
            &json!({
                "checkpoint_profile": "sd15",
                "parameters": {"checkpoint": SD15, "prompt": "a figure"}
            }),
        )
        .await;
    assert!(sd15.valid, "{:?}", sd15.parameter_errors);
    assert_eq!(
        effective(&sd15, "steps"),
        ("25".into(), WorkflowValueSource::CheckpointProfile)
    );
    assert_eq!(
        effective(&sd15, "cfg"),
        ("7.5".into(), WorkflowValueSource::CheckpointProfile)
    );

    let mismatch = api
        .request(Method::POST, path)
        .json(&json!({
            "checkpoint_profile": "sd15",
            "parameters": {"checkpoint": SDXL, "prompt": "a figure"}
        }))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(mismatch, StatusCode::UNPROCESSABLE_ENTITY).await;
    api.shutdown().await;
}

#[tokio::test]
async fn portrait_and_turnaround_runs_reach_comfyui_with_their_layouts_and_exact_values() {
    let api = start().await;
    let submit = |key: &'static str, body: Value| {
        let request = api
            .request(Method::POST, "/api/v1/runs")
            .header("Idempotency-Key", key)
            .json(&body);
        async move { request.send().await.unwrap() }
    };

    let missing_checkpoint = submit(
        "no-checkpoint",
        json!({"workflow": "portrait", "parameters": {"prompt": "a figure"}}),
    )
    .await;
    ApiHarness::assert_problem(missing_checkpoint, StatusCode::UNPROCESSABLE_ENTITY).await;
    let runs: Value = api.get_json("/api/v1/runs").await;
    assert!(runs["runs"].as_array().unwrap().is_empty());

    let portrait = submit(
        "portrait-1",
        json!({
            "workflow": "portrait", "preset": "portrait-2x3", "checkpoint_profile": "sdxl-base",
            "parameters": {"checkpoint": SDXL, "prompt": "a figure in a field"}
        }),
    )
    .await;
    assert_eq!(portrait.status(), StatusCode::ACCEPTED);
    let run: RunResponse = portrait.json().await.unwrap();
    assert_eq!(run.parameters["cfg"].to_string(), "7.0");
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    assert_eq!(graph["4"]["inputs"]["ckpt_name"], SDXL);
    assert_eq!(graph["5"]["inputs"]["width"], 832);
    assert_eq!(graph["5"]["inputs"]["height"], 1248);
    assert_eq!(graph["3"]["inputs"]["steps"], 30);
    assert_eq!(graph["3"]["inputs"]["cfg"].to_string(), "7.0");
    assert_eq!(graph["6"]["inputs"]["text"], "a figure in a field");

    let turnaround = submit(
        "turnaround-1",
        json!({
            "workflow": "turnaround", "preset": "turnaround-3-view",
            "parameters": {"checkpoint": SDXL, "prompt": "a knight, character turnaround sheet"}
        }),
    )
    .await;
    assert_eq!(turnaround.status(), StatusCode::ACCEPTED);
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    assert_eq!(graph["5"]["inputs"]["width"], 1536);
    assert_eq!(graph["5"]["inputs"]["height"], 768);
    assert_eq!(
        graph["9"]["inputs"]["filename_prefix"],
        "character-art-turnaround"
    );
    api.shutdown().await;
}
