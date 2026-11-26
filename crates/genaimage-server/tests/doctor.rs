mod common;

use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use common::{install_workflow, ApiHarness};
use genaimage_api::{client::Client, DoctorCheck, DoctorReport, DoctorStatus};
use genaimage_core::{
    doctor::{self, DoctorOptions, CHECK_IDS},
    LlmRoles,
};
use genaimage_llm::{OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions};
use genaimage_test_support::{FakeLlmScript, FakeLlmServer};
use serde_json::json;

const SECRET: &str = "sk-doctor-secret-4711";

fn models(vision: bool) -> serde_json::Value {
    json!({"object": "list", "data": [
        {"id": "assistant", "supports_tools": true, "supports_vision": false},
        {"id": "looker", "supports_tools": true, "supports_vision": vision},
    ]})
}

async fn llm(script: FakeLlmScript) -> (FakeLlmServer, String) {
    let server = FakeLlmServer::start(script).await;
    let url = server.base_url();
    (server, url)
}

async fn configured(url: &str, extra: &[(&str, &str)]) -> ApiHarness {
    let mut env = extra.to_vec();
    env.push(("GENAIMAGE_LLM_URL", url));
    if !extra
        .iter()
        .any(|(name, _)| *name == "GENAIMAGE_LLM_TEXT_MODEL")
    {
        env.push(("GENAIMAGE_LLM_TEXT_MODEL", "assistant"));
    }
    let api = ApiHarness::start_with_config(5, &env).await;
    install_workflow(&api);
    api
}

async fn report(api: &ApiHarness) -> DoctorReport {
    Client::with_base_url(&api.origin)
        .unwrap()
        .doctor()
        .await
        .unwrap()
}

fn check<'a>(report: &'a DoctorReport, id: &str) -> &'a DoctorCheck {
    report
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check {id}"))
}

fn assert_failing(report: &DoctorReport, id: &str, status: DoctorStatus, fix_names: &str) {
    let found = check(report, id);
    assert_eq!(found.status, status, "{id}: {}", found.summary);
    let fix = found.fix.as_deref().unwrap_or_default();
    assert!(
        fix.contains(fix_names),
        "{id} fix should name {fix_names}: {fix}"
    );
    assert!(!found.summary.is_empty());
}

fn direct_roles(api: &ApiHarness) -> LlmRoles {
    let config = &api.core.config().llm;
    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        allowed_hosts: config.allowed_hosts.clone(),
        connect_timeout: Duration::from_secs(2),
        first_byte_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(2),
    })
    .unwrap();
    LlmRoles::new(
        config,
        Some(Arc::new(provider) as Arc<dyn genaimage_core::LlmProvider>),
    )
}

fn generous() -> DoctorOptions {
    DoctorOptions {
        disk_warning_bytes: 0,
        disk_error_bytes: 0,
        ..DoctorOptions::default()
    }
}

#[tokio::test]
async fn a_healthy_setup_reports_every_check_ok_without_fixes() {
    let (_llm, url) = llm(FakeLlmScript::default().with_models_response(models(true))).await;
    let api = configured(&url, &[("GENAIMAGE_LLM_VISION_MODEL", "looker")]).await;
    let report = doctor::run(&api.core, &direct_roles(&api), &[], &generous()).await;
    let ids: Vec<&str> = report
        .checks
        .iter()
        .map(|check| check.id.as_str())
        .collect();
    assert_eq!(ids, CHECK_IDS);
    for check in &report.checks {
        assert_eq!(
            (check.status, &check.fix),
            (doctor::DoctorStatus::Ok, &None),
            "{}: {}",
            check.id,
            check.summary
        );
    }
    api.shutdown().await;
}

#[tokio::test]
async fn the_endpoint_returns_the_fixed_check_list() {
    let api = ApiHarness::start().await;
    let report = report(&api).await;
    let ids: Vec<&str> = report
        .checks
        .iter()
        .map(|check| check.id.as_str())
        .collect();
    assert_eq!(ids, CHECK_IDS);
    assert_eq!(check(&report, "server").status, DoctorStatus::Ok);
    for check in &report.checks {
        assert_eq!(
            check.fix.is_none(),
            check.status == DoctorStatus::Ok,
            "{}",
            check.id
        );
    }
    api.shutdown().await;
}

#[tokio::test]
async fn an_unreachable_comfyui_fails_reachability_and_skips_dependants() {
    let api = ApiHarness::start().await;
    api.fake.health_unavailable.store(true, Ordering::SeqCst);
    let report = report(&api).await;
    assert_failing(
        &report,
        "comfyui.reachable",
        DoctorStatus::Error,
        "comfyui.base_url",
    );
    for id in [
        "comfyui.version",
        "comfyui.object_info",
        "workflows.nodes",
        "gpu",
    ] {
        let skipped = check(&report, id);
        assert_eq!(skipped.status, DoctorStatus::Skipped, "{id}");
        assert!(
            skipped
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("comfyui.")),
            "{id}"
        );
    }
    api.shutdown().await;
}

#[tokio::test]
async fn a_failing_object_info_is_an_error_and_workflow_nodes_are_skipped() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    api.fake
        .object_info_unavailable
        .store(true, Ordering::SeqCst);
    let report = report(&api).await;
    assert_eq!(check(&report, "comfyui.reachable").status, DoctorStatus::Ok);
    assert_failing(
        &report,
        "comfyui.object_info",
        DoctorStatus::Error,
        "ComfyUI console",
    );
    assert_eq!(
        check(&report, "workflows.nodes").status,
        DoctorStatus::Skipped
    );
    api.shutdown().await;
}

#[tokio::test]
async fn a_workflow_node_missing_from_object_info_is_listed_with_an_install_fix() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    *api.fake.missing_classes.lock().unwrap() = vec!["KSampler".into()];
    let report = report(&api).await;
    assert_failing(
        &report,
        "workflows.nodes",
        DoctorStatus::Error,
        "custom nodes",
    );
    let nodes = check(&report, "workflows.nodes");
    assert_eq!(nodes.items, ["contract-workflow: missing KSampler"]);
    assert!(nodes.summary.starts_with("1 of 1"));
    api.shutdown().await;
}

#[tokio::test]
async fn missing_nodes_in_some_workflows_are_a_warning_and_the_list_is_bounded() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let dir = api.core.config().workflows.clone();
    let graph = (0..15)
        .map(|n| {
            (
                n.to_string(),
                json!({"class_type": format!("Absent{n:02}"), "inputs": {}}),
            )
        })
        .chain([(
            "99".to_owned(),
            json!({"class_type": "SaveImage", "inputs": {}}),
        )])
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        dir.join("many.api.json"),
        serde_json::to_vec(&graph).unwrap(),
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("contract.genaimage.json")).unwrap())
            .unwrap();
    manifest["id"] = json!("many");
    manifest["workflow"] = json!("many.api.json");
    manifest["parameters"] = json!({});
    manifest["presets"] = json!({});
    manifest["outputs"] = json!([{"node": "99", "kind": "image"}]);
    std::fs::write(
        dir.join("many.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let report = report(&api).await;
    assert_failing(
        &report,
        "workflows.nodes",
        DoctorStatus::Warning,
        "custom nodes",
    );
    let nodes = check(&report, "workflows.nodes");
    assert_eq!(nodes.items.len(), 1, "{:?}", nodes.items);
    assert!(
        nodes.items[0].starts_with("many: missing Absent00, ")
            && nodes.items[0].ends_with("(+5 more)")
    );
    api.shutdown().await;
}

#[tokio::test]
async fn an_unconfigured_llm_is_skipped_with_an_optional_fix() {
    let api = ApiHarness::start().await;
    let report = report(&api).await;
    for id in ["llm.reachable", "llm.text", "llm.vision"] {
        let skipped = check(&report, id);
        assert_eq!(skipped.status, DoctorStatus::Skipped, "{id}");
        assert!(
            skipped.fix.as_deref().unwrap().contains("llm.base_url"),
            "{id}"
        );
    }
    assert!(check(&report, "llm.reachable")
        .fix
        .as_deref()
        .unwrap()
        .contains("optional"));
    api.shutdown().await;
}

#[tokio::test]
async fn a_missing_llm_model_names_the_setting_and_the_available_models() {
    let (_llm, url) = llm(FakeLlmScript::default().with_models_response(models(true))).await;
    let api = configured(&url, &[("GENAIMAGE_LLM_TEXT_MODEL", "ghost")]).await;
    let report = report(&api).await;
    assert_eq!(check(&report, "llm.reachable").status, DoctorStatus::Ok);
    assert_failing(&report, "llm.text", DoctorStatus::Error, "llm.text_model");
    assert!(check(&report, "llm.text")
        .fix
        .as_deref()
        .unwrap()
        .contains("assistant"));
    assert!(check(&report, "llm.text").summary.contains("ghost"));
    api.shutdown().await;
}

#[tokio::test]
async fn a_vision_role_without_image_capability_is_an_error() {
    let (_llm, url) = llm(FakeLlmScript::default().with_models_response(models(false))).await;
    let api = configured(&url, &[("GENAIMAGE_LLM_VISION_MODEL", "looker")]).await;
    let report = report(&api).await;
    assert_eq!(check(&report, "llm.text").status, DoctorStatus::Ok);
    assert_failing(
        &report,
        "llm.vision",
        DoctorStatus::Error,
        "[llm.capabilities.\"looker\"]",
    );
    assert!(check(&report, "llm.vision")
        .fix
        .as_deref()
        .unwrap()
        .contains("llm.vision_model"));
    api.shutdown().await;
}

#[tokio::test]
async fn an_llm_that_fails_its_model_listing_skips_the_role_checks() {
    let (_llm, url) =
        llm(FakeLlmScript::default().with_models_error(500, "backend exploded")).await;
    let api = configured(&url, &[]).await;
    let report = report(&api).await;
    assert_failing(
        &report,
        "llm.reachable",
        DoctorStatus::Error,
        "llm.base_url",
    );
    for id in ["llm.text", "llm.vision"] {
        assert_failing(&report, id, DoctorStatus::Skipped, "llm.reachable");
    }
    api.shutdown().await;
}

#[tokio::test]
async fn a_cpu_only_comfyui_gets_a_gpu_warning() {
    let api = ApiHarness::start().await;
    *api.fake.devices.lock().unwrap() = json!([{"name": "cpu", "type": "cpu"}]);
    let report = report(&api).await;
    assert_failing(&report, "gpu", DoctorStatus::Warning, "PyTorch");
    assert!(check(&report, "gpu").summary.contains("CPU only"));
    api.shutdown().await;
}

#[tokio::test]
async fn low_disk_space_names_the_runs_directory() {
    let api = ApiHarness::start().await;
    let options = DoctorOptions {
        disk_warning_bytes: u64::MAX,
        disk_error_bytes: u64::MAX,
        ..generous()
    };
    let report = doctor::run(
        &api.core,
        &LlmRoles::new(&api.core.config().llm, None),
        &[],
        &options,
    )
    .await;
    let disk = report
        .checks
        .iter()
        .find(|check| check.id == "disk")
        .unwrap();
    assert_eq!(disk.status, doctor::DoctorStatus::Error);
    assert!(disk
        .fix
        .as_deref()
        .unwrap()
        .contains(&api.core.config().runs.display().to_string()));
    api.shutdown().await;
}

#[tokio::test]
async fn pending_settings_are_a_config_warning_naming_the_keys() {
    let api = ApiHarness::start().await;
    let restart = ["comfyui.base_url".to_owned()];
    let report = doctor::run(
        &api.core,
        &LlmRoles::new(&api.core.config().llm, None),
        &restart,
        &generous(),
    )
    .await;
    let config = report
        .checks
        .iter()
        .find(|check| check.id == "config")
        .unwrap();
    assert_eq!(config.status, doctor::DoctorStatus::Warning);
    assert!(config.fix.as_deref().unwrap().contains("comfyui.base_url"));
    api.shutdown().await;
}

#[tokio::test]
async fn the_llm_key_never_appears_in_the_report_even_if_the_provider_echoes_it() {
    let echo = format!("invalid api key {SECRET}");
    let (_llm, url) = llm(FakeLlmScript::default().with_models_error(401, echo)).await;
    let api = configured(&url, &[("GENAIMAGE_LLM_API_KEY", SECRET)]).await;
    let report = report(&api).await;
    assert_eq!(check(&report, "llm.reachable").status, DoctorStatus::Error);
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(!rendered.contains(SECRET), "{rendered}");
    let direct = doctor::run(&api.core, &direct_roles(&api), &[], &generous()).await;
    assert!(
        !format!("{direct:?}").contains(SECRET)
            && !serde_json::to_string(&direct).unwrap().contains(SECRET)
    );
    api.shutdown().await;
}
