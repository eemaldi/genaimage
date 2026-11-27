mod common;

use std::fs;

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    client::Client, PackListResponse, PackRequirementStatus, PackValidationResponse,
    PackValidationStatus, WorkflowListResponse,
};
use genaimage_core::doctor::{self, DoctorOptions};
use genaimage_core::LlmRoles;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

fn write_pack(api: &ApiHarness, id: &str, workflow_id: Option<&str>, extra: &str) {
    let dir = api.core.config().packs[0].join(id);
    fs::create_dir_all(&dir).unwrap();
    let workflows = workflow_id
        .map(|_| "workflows = ['wf.genaimage.json']\n")
        .unwrap_or_default();
    fs::write(
        dir.join("pack.toml"),
        format!("id = '{id}'\nversion = '1.0.0'\ndescription = 'Pack {id}'\n{workflows}{extra}"),
    )
    .unwrap();
    if let Some(workflow_id) = workflow_id {
        fs::write(
            dir.join("workflow.api.json"),
            include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
        )
        .unwrap();
        let mut manifest: Value = serde_json::from_str(include_str!(
            "../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"
        ))
        .unwrap();
        manifest["id"] = json!(workflow_id);
        fs::write(
            dir.join("wf.genaimage.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
}

fn object_info() -> Value {
    json!({
        "KSampler": {},
        "CheckpointLoaderSimple": {"input": {"required": {
            "ckpt_name": [["base.safetensors"]]
        }}},
        "LoraLoader": {"input": {"required": {
            "lora_name": ["COMBO", {"options": ["style.safetensors"]}]
        }}}
    })
}

const REQUIRES: &str = "\n[requires]\nnode_classes = ['KSampler', 'Absent']\n\n[requires.models]\ncheckpoints = ['base.safetensors', 'gone.safetensors']\nloras = ['style.safetensors']\nembeddings = ['e.pt']\n";

#[tokio::test]
async fn packs_list_their_contents_and_isolate_broken_packs() {
    let api = ApiHarness::start().await;
    write_pack(
        &api,
        "demo",
        Some("demo-flow"),
        &format!("templates = ['t/a.md', 'gone.md']\n{REQUIRES}"),
    );
    fs::create_dir_all(api.core.config().packs[0].join("demo/t")).unwrap();
    fs::write(api.core.config().packs[0].join("demo/t/a.md"), "x").unwrap();
    let broken = api.core.config().packs[0].join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(
        broken.join("pack.toml"),
        "id = 'broken'\nworkflows = ['../x']\n",
    )
    .unwrap();
    let list: PackListResponse = api.get_json("/api/v1/packs").await;
    assert_eq!(list.packs.len(), 1);
    let pack = &list.packs[0];
    assert_eq!(
        (
            pack.id.as_str(),
            pack.version.as_str(),
            pack.description.as_str()
        ),
        ("demo", "1.0.0", "Pack demo")
    );
    assert_eq!(pack.workflow_ids, ["demo-flow"]);
    assert_eq!(pack.templates, ["t/a.md", "gone.md"]);
    assert_eq!(pack.requires.node_classes, ["KSampler", "Absent"]);
    assert_eq!(pack.requires.models["loras"], ["style.safetensors"]);
    assert_eq!(pack.root, api.core.config().packs[0].display().to_string());
    assert_eq!(pack.diagnostics.len(), 1);
    assert!(pack.diagnostics[0].contains("gone.md"));
    assert_eq!(list.diagnostics.len(), 1);
    assert!(list.diagnostics[0].path.ends_with("broken/pack.toml"));
    api.shutdown().await;
}

#[tokio::test]
async fn workflows_carry_sources_and_duplicates_are_unavailable_not_shadowed() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    write_pack(&api, "demo", Some("demo-flow"), "");
    write_pack(&api, "twin", Some("contract-workflow"), "");
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    let sources: Vec<_> = list
        .workflows
        .iter()
        .map(|w| (w.id.as_str(), w.source.as_str()))
        .collect();
    assert_eq!(
        sources,
        [
            ("contract-workflow", "user"),
            ("contract-workflow", "pack:twin"),
            ("demo-flow", "pack:demo")
        ],
        "both claimants stay visible, each with its source"
    );
    assert!(list
        .diagnostics
        .iter()
        .any(|d| d.error.contains("declared more than once")
            && d.error.contains("user '")
            && d.error.contains("pack:twin '")));
    let response = api
        .request(Method::GET, "/api/v1/workflows/contract-workflow")
        .send()
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
    api.shutdown().await;
}

#[tokio::test]
async fn a_new_pack_appears_on_refresh_without_restart() {
    let api = ApiHarness::start().await;
    let before: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert!(before.workflows.is_empty() && before.diagnostics.is_empty());
    write_pack(&api, "late", Some("late-flow"), "");
    let refreshed: WorkflowListResponse =
        api.post_json("/api/v1/workflows/refresh", &json!({})).await;
    assert_eq!(refreshed.workflows.len(), 1);
    assert_eq!(refreshed.workflows[0].source, "pack:late");
    let details = api
        .request(Method::GET, "/api/v1/workflows/late-flow")
        .send()
        .await
        .unwrap();
    assert_eq!(details.status(), StatusCode::OK);
    api.shutdown().await;
}

async fn validate(api: &ApiHarness, id: &str) -> PackValidationResponse {
    api.get_json(&format!("/api/v1/packs/{id}/validate")).await
}

#[tokio::test]
async fn validation_reports_missing_present_and_unknown_requirements() {
    let api = ApiHarness::start().await;
    write_pack(&api, "demo", None, REQUIRES);
    *api.fake.object_info_override.lock().unwrap() = Some(object_info());
    let result = validate(&api, "demo").await;
    assert_eq!(result.status, PackValidationStatus::Missing);
    assert_eq!(result.reason, None);
    let nodes: Vec<_> = result
        .node_classes
        .iter()
        .map(|n| (n.class.as_str(), n.status))
        .collect();
    assert_eq!(
        nodes,
        [
            ("KSampler", PackRequirementStatus::Available),
            ("Absent", PackRequirementStatus::Missing)
        ]
    );
    let models: Vec<_> = result
        .models
        .iter()
        .map(|m| (m.folder.as_str(), m.name.as_str(), m.status))
        .collect();
    assert_eq!(
        models,
        [
            (
                "checkpoints",
                "base.safetensors",
                PackRequirementStatus::Available
            ),
            (
                "checkpoints",
                "gone.safetensors",
                PackRequirementStatus::Missing
            ),
            ("embeddings", "e.pt", PackRequirementStatus::Unknown),
            (
                "loras",
                "style.safetensors",
                PackRequirementStatus::Available
            ),
        ]
    );
    assert_eq!(
        result.models[3].loader.as_deref(),
        Some("LoraLoader.lora_name")
    );
    assert!(result.models[2].loader.is_none() && result.models[2].reason.is_some());
    api.shutdown().await;
}

#[tokio::test]
async fn validation_never_reports_success_when_the_backend_is_unreachable() {
    let api = ApiHarness::start().await;
    write_pack(&api, "demo", None, REQUIRES);
    api.fake
        .object_info_unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = validate(&api, "demo").await;
    assert_eq!(result.status, PackValidationStatus::Unavailable);
    assert!(result.reason.is_some());
    assert!(result
        .node_classes
        .iter()
        .all(|n| n.status == PackRequirementStatus::Unknown));
    assert!(result
        .models
        .iter()
        .all(|m| m.status == PackRequirementStatus::Unknown));
    api.shutdown().await;
}

#[tokio::test]
async fn validating_an_unknown_pack_is_not_found_and_installs_nothing() {
    let api = ApiHarness::start().await;
    let response = api
        .request(Method::GET, "/api/v1/packs/absent/validate")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    api.shutdown().await;
}

#[tokio::test]
async fn doctor_uses_the_same_requirement_check_as_the_endpoint() {
    let api = ApiHarness::start().await;
    write_pack(
        &api,
        "demo",
        None,
        "\n[requires]\nnode_classes = ['Absent']\n",
    );
    *api.fake.object_info_override.lock().unwrap() = Some(object_info());
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let report = doctor::run(&api.core, &roles, &[], &DoctorOptions::default()).await;
    let check = report
        .checks
        .iter()
        .find(|check| check.id == doctor::PACKS_REQUIREMENTS)
        .unwrap();
    assert_eq!(check.status, doctor::DoctorStatus::Error);
    assert_eq!(check.items, ["demo: missing node class Absent"]);
    assert!(check.fix.as_deref().unwrap().contains("never installs"));
    let validation = validate(&api, "demo").await;
    assert_eq!(validation.status, PackValidationStatus::Missing);

    write_pack(
        &api,
        "demo",
        None,
        "\n[requires]\nnode_classes = ['KSampler']\n",
    );
    let report = doctor::run(&api.core, &roles, &[], &DoctorOptions::default()).await;
    let check = report
        .checks
        .iter()
        .find(|check| check.id == doctor::PACKS_REQUIREMENTS)
        .unwrap();
    assert_eq!(check.status, doctor::DoctorStatus::Ok);
    api.shutdown().await;
}

#[tokio::test]
async fn doctor_warns_instead_of_ok_for_unknown_folders() {
    let api = ApiHarness::start().await;
    write_pack(
        &api,
        "demo",
        None,
        "\n[requires.models]\nembeddings = ['e.pt']\n",
    );
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let report = doctor::run(&api.core, &roles, &[], &DoctorOptions::default()).await;
    let check = report
        .checks
        .iter()
        .find(|check| check.id == doctor::PACKS_REQUIREMENTS)
        .unwrap();
    assert_eq!(check.status, doctor::DoctorStatus::Warning);
    api.shutdown().await;
}

#[tokio::test]
async fn the_client_reaches_the_pack_routes() {
    let api = ApiHarness::start().await;
    write_pack(&api, "demo", Some("demo-flow"), "");
    let client = Client::with_base_url(&api.origin).unwrap();
    assert_eq!(client.packs().await.unwrap().packs[0].id, "demo");
    assert_eq!(client.refresh_workflows().await.unwrap().workflows.len(), 1);
    assert_eq!(
        client.validate_pack("demo").await.unwrap().status,
        PackValidationStatus::Ok
    );
    api.shutdown().await;
}
