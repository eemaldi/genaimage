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
    for workflow in list
        .workflows
        .iter()
        .filter(|w| w.id == "contract-workflow")
    {
        assert!(!workflow.available);
        let reason = workflow.unavailable_reason.as_deref().unwrap();
        let other = if workflow.source == "user" {
            "pack:twin '"
        } else {
            "user '"
        };
        assert!(reason.contains(other), "{reason}");
    }
    let unique = list.workflows.iter().find(|w| w.id == "demo-flow").unwrap();
    assert!(unique.available && unique.unavailable_reason.is_none());
    api.shutdown().await;
}

const AMBIGUOUS: &str = "urn:genaimage:problem:workflow-ambiguous";

async fn assert_ambiguous(response: reqwest::Response) {
    let problem = ApiHarness::assert_typed_problem(response, StatusCode::CONFLICT, AMBIGUOUS).await;
    assert!(
        problem.detail.contains("user '") && problem.detail.contains("pack:twin '"),
        "every claimant is named: {}",
        problem.detail
    );
}

#[tokio::test]
async fn a_duplicate_id_is_a_409_on_details_validate_thumbnail_submit_and_rerun() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let source: Value = api
        .request(Method::POST, "/api/v1/runs")
        .header("Idempotency-Key", "original")
        .json(
            &json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 1}}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompts = api
        .fake
        .prompt_calls
        .load(std::sync::atomic::Ordering::SeqCst);
    write_pack(&api, "twin", Some("contract-workflow"), "");
    fs::write(api.core.config().packs[0].join("twin/t.png"), b"png").unwrap();
    let url = "/api/v1/workflows/contract-workflow";
    assert_ambiguous(api.request(Method::GET, url).send().await.unwrap()).await;
    assert_ambiguous(
        api.request(Method::GET, &format!("{url}/thumbnail"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_ambiguous(
        api.request(Method::POST, &format!("{url}/validate"))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_ambiguous(
        api.request(Method::POST, "/api/v1/runs")
            .header("Idempotency-Key", "dup-submit")
            .json(&json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 2}}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let rerun = format!("/api/v1/runs/{}/rerun", source["id"].as_str().unwrap());
    assert_ambiguous(
        api.request(Method::POST, &rerun)
            .header("Idempotency-Key", "dup-rerun")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        api.fake
            .prompt_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        prompts,
        "nothing was submitted"
    );
    api.shutdown().await;
}

#[tokio::test]
async fn a_run_recorded_from_the_user_root_reruns_after_the_workflow_moves_into_a_pack() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let source: Value = api
        .request(Method::POST, "/api/v1/runs")
        .header("Idempotency-Key", "original")
        .json(
            &json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 1}}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let root = api.core.config().workflows.clone();
    let pack = api.core.config().packs[0].join("moved");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("pack.toml"),
        "id = 'moved'\nversion = '1'\nworkflows = ['contract.genaimage.json']\n",
    )
    .unwrap();
    for name in ["contract.genaimage.json", "workflow.api.json"] {
        fs::copy(root.join(name), pack.join(name)).unwrap();
        fs::remove_file(root.join(name)).unwrap();
    }
    let listing: WorkflowListResponse =
        api.post_json("/api/v1/workflows/refresh", &json!({})).await;
    assert_eq!(listing.workflows.len(), 1);
    assert_eq!(listing.workflows[0].source, "pack:moved");
    let response = api
        .request(
            Method::POST,
            &format!("/api/v1/runs/{}/rerun", source["id"].as_str().unwrap()),
        )
        .header("Idempotency-Key", "after-move")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let rerun: Value = response.json().await.unwrap();
    assert_eq!(rerun["rerun_of"], source["id"]);
    assert_eq!(
        api.fake
            .prompt_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pack_toml_symlinked_outside_is_never_read_or_echoed() {
    let api = ApiHarness::start().await;
    let secret = api.workspace().join("secret.txt");
    fs::write(&secret, "TOPSECRET = [not toml").unwrap();
    let dir = api.core.config().packs[0].join("evil");
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&secret, dir.join("pack.toml")).unwrap();
    let list: PackListResponse = api.get_json("/api/v1/packs").await;
    let workflows: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert!(list.packs.is_empty());
    let diagnostics = list.diagnostics.iter().chain(workflows.diagnostics.iter());
    let texts: Vec<_> = diagnostics.map(|d| d.error.as_str()).collect();
    assert!(texts
        .iter()
        .any(|t| t.contains("outside the pack directory")));
    assert!(texts.iter().all(|t| !t.contains("TOPSECRET")), "{texts:?}");
    assert!(
        texts
            .iter()
            .all(|t| !t.contains("workflow pack invalid: workflow pack invalid")),
        "{texts:?}"
    );
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pack_graph_symlinked_outside_is_rejected_everywhere_before_it_is_read() {
    let api = ApiHarness::start().await;
    write_pack(&api, "leaky", Some("leaky-flow"), "");
    let dir = api.core.config().packs[0].join("leaky");
    let outside = api.workspace().join("outside.json");
    fs::write(&outside, "OUTSIDE-SECRET").unwrap();
    fs::remove_file(dir.join("workflow.api.json")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("workflow.api.json")).unwrap();
    let list: PackListResponse = api.get_json("/api/v1/packs").await;
    let pack = &list.packs[0];
    assert!(pack.workflow_ids.is_empty(), "{:?}", pack.workflow_ids);
    assert_eq!(pack.diagnostics.len(), 1);
    assert!(pack.diagnostics[0].contains("outside the pack directory"));
    assert!(!pack.diagnostics[0].contains("OUTSIDE-SECRET"));
    let workflows: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert!(workflows.workflows.is_empty());
    let validation = validate(&api, "leaky").await;
    assert_eq!(validation.status, PackValidationStatus::Missing);
    assert_eq!(validation.diagnostics, pack.diagnostics);
    api.shutdown().await;
}

#[tokio::test]
async fn pack_listing_and_validation_report_duplicate_workflow_ids_and_never_say_ok() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    write_pack(&api, "twin", Some("contract-workflow"), "");
    let list: PackListResponse = api.get_json("/api/v1/packs").await;
    assert_eq!(list.packs[0].workflow_ids, ["contract-workflow"]);
    assert_eq!(list.packs[0].diagnostics.len(), 1);
    assert!(list.packs[0].diagnostics[0].contains("also declared by user '"));
    let validation = validate(&api, "twin").await;
    assert_eq!(validation.status, PackValidationStatus::Missing);
    assert_eq!(validation.diagnostics, list.packs[0].diagnostics);
    api.shutdown().await;
}

#[tokio::test]
async fn a_pack_without_requirements_is_not_ok_while_a_declared_file_is_missing() {
    let api = ApiHarness::start().await;
    write_pack(&api, "thin", None, "templates = ['gone.md']\n");
    let validation = validate(&api, "thin").await;
    assert_eq!(validation.status, PackValidationStatus::Missing);
    assert_eq!(validation.diagnostics.len(), 1);
    assert!(validation.diagnostics[0].contains("gone.md"));
    write_pack(&api, "fine", Some("fine-flow"), "");
    let fine = validate(&api, "fine").await;
    assert_eq!(fine.status, PackValidationStatus::Ok);
    assert!(fine.diagnostics.is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn doctor_reports_pack_problems_even_when_node_classes_are_unknown() {
    let api = ApiHarness::start().await;
    write_pack(
        &api,
        "thin",
        None,
        "templates = ['gone.md']\n[requires]\nnode_classes = ['KSampler']\n",
    );
    api.fake
        .object_info_unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let report = doctor::run(&api.core, &roles, &[], &DoctorOptions::default()).await;
    let check = report
        .checks
        .iter()
        .find(|check| check.id == doctor::PACKS_REQUIREMENTS)
        .unwrap();
    assert_eq!(check.status, doctor::DoctorStatus::Warning);
    assert!(check.items.iter().any(|item| item.contains("gone.md")));
    assert!(check.items.iter().all(|item| !item.contains("KSampler")));
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn pack_thumbnails_are_confined_to_the_pack_directory() {
    let api = ApiHarness::start().await;
    for (id, workflow) in [("alpha", "alpha-flow"), ("beta", "beta-flow")] {
        write_pack(&api, id, Some(workflow), "");
        let dir = api.core.config().packs[0].join(id);
        let path = dir.join("wf.genaimage.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["thumbnail"] = json!("thumb.png");
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    let packs = api.core.config().packs[0].clone();
    fs::write(packs.join("beta/thumb.png"), b"beta-image").unwrap();
    std::os::unix::fs::symlink(packs.join("beta/thumb.png"), packs.join("alpha/thumb.png"))
        .unwrap();
    let own = api
        .request(Method::GET, "/api/v1/workflows/beta-flow/thumbnail")
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    assert_eq!(own.bytes().await.unwrap().as_ref(), b"beta-image");
    let borrowed = api
        .request(Method::GET, "/api/v1/workflows/alpha-flow/thumbnail")
        .send()
        .await
        .unwrap();
    assert_eq!(borrowed.status(), StatusCode::NOT_FOUND);
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
