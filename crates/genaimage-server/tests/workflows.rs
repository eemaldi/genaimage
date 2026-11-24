mod common;

use std::sync::atomic::Ordering;

use common::ApiHarness;
use genaimage_api::{
    WorkflowDetails, WorkflowListResponse, WorkflowNodeCheckStatus, WorkflowValidationResponse,
};
use reqwest::{header, Method, StatusCode};
use serde_json::{json, Value};

fn install(api: &ApiHarness, thumbnail: Option<&str>) {
    let dir = &api.core.config().workflows;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("workflow.api.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
    )
    .unwrap();
    let mut manifest: Value = serde_json::from_str(include_str!(
        "../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"
    ))
    .unwrap();
    manifest["description"] = json!("Example");
    manifest["tags"] = json!(["test"]);
    manifest["category"] = json!("image");
    manifest["parameters"]["steps"]["widget"] = json!("slider");
    manifest["parameters"]["steps"]["step"] = json!(1);
    if let Some(path) = thumbnail {
        manifest["thumbnail"] = json!(path);
    }
    std::fs::write(
        dir.join("contract.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn catalogue_and_details_include_metadata_and_discovery_errors() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    std::fs::write(api.core.config().workflows.join("image.png"), b"image").unwrap();
    std::fs::write(
        api.core.config().workflows.join("invalid.genaimage.json"),
        "not json",
    )
    .unwrap();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 1);
    let summary = &list.workflows[0];
    assert_eq!(summary.id, "contract-workflow");
    assert_eq!(summary.name, "Contract workflow");
    assert_eq!(summary.description.as_deref(), Some("Example"));
    assert_eq!(
        summary.tags.as_deref(),
        Some(["test".to_owned()].as_slice())
    );
    assert_eq!(summary.category.as_deref(), Some("image"));
    assert_eq!(summary.version, "1.0.0");
    assert!(summary
        .thumbnail_url
        .as_deref()
        .unwrap()
        .ends_with("/thumbnail"));
    assert_eq!(list.diagnostics.len(), 1);
    assert!(list.diagnostics[0].path.ends_with("invalid.genaimage.json"));
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(details.schema_version, 2);
    assert_eq!(
        details.parameters["steps"].widget.as_deref(),
        Some("slider")
    );
    assert_eq!(details.parameters["steps"].step, Some(1.0));
    assert_eq!(details.presets["fast"]["seed"], 42);
    assert_eq!(details.outputs[0].node, "9");
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn broken_manifest_symlinks_do_not_hide_valid_workflows() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    std::fs::write(dir.join("image.png"), b"image").unwrap();
    std::os::unix::fs::symlink("loop.genaimage.json", dir.join("loop.genaimage.json")).unwrap();
    std::os::unix::fs::symlink("missing.json", dir.join("dangling.genaimage.json")).unwrap();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 1);
    assert_eq!(list.workflows[0].id, "contract-workflow");
    for name in ["loop.genaimage.json", "dangling.genaimage.json"] {
        let diagnostic = list
            .diagnostics
            .iter()
            .find(|item| item.path.ends_with(name))
            .expect("broken symlink diagnostic");
        assert!(!diagnostic.error.is_empty());
    }
    let response = api
        .request(
            Method::GET,
            list.workflows[0].thumbnail_url.as_deref().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap(), &b"image"[..]);
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(details.id, "contract-workflow");
    for name in ["loop.genaimage.json", "dangling.genaimage.json"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    let repaired: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(repaired.workflows.len(), 1);
    assert!(repaired.diagnostics.is_empty());
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn directory_cycle_and_unreadable_directory_are_nonfatal() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    std::fs::write(dir.join("image.png"), b"image").unwrap();
    std::os::unix::fs::symlink(".", dir.join("cycle")).unwrap();
    let inaccessible = dir.join("inaccessible");
    std::fs::create_dir(&inaccessible).unwrap();
    std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = Restore(inaccessible.clone());
    let inaccessible_is_denied = std::fs::read_dir(&inaccessible).is_err();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 1);
    assert!(list
        .diagnostics
        .iter()
        .any(|item| item.path.ends_with("cycle") && item.error.contains("cycle")));
    if inaccessible_is_denied {
        assert!(list
            .diagnostics
            .iter()
            .any(|item| item.path.ends_with("inaccessible")));
    }
    let response = api
        .request(
            Method::GET,
            list.workflows[0].thumbnail_url.as_deref().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(details.id, "contract-workflow");
    drop(restore);
    api.shutdown().await;
}

#[tokio::test]
async fn missing_thumbnail_is_a_nonfatal_diagnostic() {
    let api = ApiHarness::start().await;
    install(&api, Some("missing.png"));
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 1);
    assert!(list
        .diagnostics
        .iter()
        .any(|item| item.path.ends_with("missing.png")));
    ApiHarness::assert_problem(
        api.request(
            Method::GET,
            list.workflows[0].thumbnail_url.as_deref().unwrap(),
        )
        .send()
        .await
        .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(details.id, "contract-workflow");
    api.shutdown().await;
}

#[tokio::test]
async fn broken_graph_reference_does_not_hide_valid_workflows() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    std::fs::write(dir.join("image.png"), b"image").unwrap();
    std::fs::write(dir.join("not-a-directory"), b"file").unwrap();
    let mut broken: Value =
        serde_json::from_slice(&std::fs::read(dir.join("contract.genaimage.json")).unwrap())
            .unwrap();
    broken["id"] = json!("broken");
    broken["workflow"] = json!("not-a-directory/graph.json");
    std::fs::write(
        dir.join("broken.genaimage.json"),
        serde_json::to_vec(&broken).unwrap(),
    )
    .unwrap();

    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 1);
    assert_eq!(list.workflows[0].id, "contract-workflow");
    assert_eq!(list.diagnostics.len(), 1);
    assert!(list.diagnostics[0].path.ends_with("broken.genaimage.json"));
    assert!(list.diagnostics[0]
        .error
        .contains("not-a-directory/graph.json"));
    let response = api
        .request(
            Method::GET,
            list.workflows[0].thumbnail_url.as_deref().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap(), &b"image"[..]);
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(details.id, "contract-workflow");

    std::fs::remove_file(dir.join("not-a-directory")).unwrap();
    std::fs::create_dir(dir.join("not-a-directory")).unwrap();
    std::fs::copy(
        dir.join("workflow.api.json"),
        dir.join("not-a-directory/graph.json"),
    )
    .unwrap();
    let repaired: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(repaired.workflows.len(), 2);
    assert!(repaired.diagnostics.is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn detail_reports_declared_and_effective_values_additively() {
    let api = ApiHarness::start().await;
    install(&api, None);
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["parameters"]["seed"]
        .as_object_mut()
        .unwrap()
        .remove("required");
    manifest["parameters"]["steps"]["required"] = json!(false);
    manifest["outputs"] = json!([{"node":"9"}, {"node":"9", "kind":"image"}]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let raw: Value = api.get_json("/api/v1/workflows/contract-workflow").await;
    assert_eq!(raw["parameters"]["seed"]["required"], false);
    assert_eq!(raw["parameters"]["seed"]["declared_required"], Value::Null);
    assert_eq!(raw["parameters"]["steps"]["required"], false);
    assert_eq!(raw["parameters"]["steps"]["declared_required"], false);
    assert_eq!(raw["outputs"][0]["kind"], "image");
    assert_eq!(raw["outputs"][0]["declared_kind"], Value::Null);
    assert_eq!(raw["outputs"][1]["declared_kind"], "image");
    let typed: WorkflowDetails = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(
        typed.parameters["steps"].declared_required,
        Some(Some(false))
    );
    assert_eq!(typed.parameters["seed"].declared_required, Some(None));
    assert_eq!(typed.outputs[0].declared_kind, Some(None));
    assert_eq!(
        typed.outputs[1].declared_kind,
        Some(Some("image".to_owned()))
    );
    let mut old = raw;
    for parameter in old["parameters"].as_object_mut().unwrap().values_mut() {
        parameter
            .as_object_mut()
            .unwrap()
            .remove("declared_required");
    }
    for output in old["outputs"].as_array_mut().unwrap() {
        output.as_object_mut().unwrap().remove("declared_kind");
    }
    let old: WorkflowDetails = serde_json::from_value(old).unwrap();
    assert_eq!(old.parameters["seed"].declared_required, None);
    assert_eq!(old.outputs[0].declared_kind, None);
    let inspected = serde_json::to_value(old).unwrap();
    assert!(inspected["parameters"]["seed"]
        .get("declared_required")
        .is_none());
    assert!(inspected["outputs"][0].get("declared_kind").is_none());
    api.shutdown().await;
}

#[tokio::test]
async fn validates_parameters_presets_and_missing_nodes_without_submission() {
    let api = ApiHarness::start().await;
    install(&api, None);
    let path = "/api/v1/workflows/contract-workflow/validate";
    let valid: WorkflowValidationResponse = api.post_json(path, &json!({"preset":"fast"})).await;
    assert!(valid.valid);
    assert_eq!(valid.node_check.status, WorkflowNodeCheckStatus::Available);
    let overridden: WorkflowValidationResponse = api
        .post_json(path, &json!({"preset":"fast", "parameters":{"steps":0}}))
        .await;
    assert!(!overridden.valid);
    assert_eq!(overridden.parameter_errors[0].parameter, "steps");
    let invalid: WorkflowValidationResponse = api
        .post_json(
            path,
            &json!({"parameters":{"seed":"wrong", "steps":101, "extra":1}}),
        )
        .await;
    assert!(!invalid.valid);
    assert_eq!(invalid.parameter_errors.len(), 3);
    for name in ["seed", "steps", "extra"] {
        assert!(invalid
            .parameter_errors
            .iter()
            .any(|e| e.parameter == name && !e.reason.is_empty()));
    }
    let required: WorkflowValidationResponse = api.post_json(path, &json!({})).await;
    assert_eq!(required.parameter_errors.len(), 2);
    api.fake
        .missing_classes
        .lock()
        .unwrap()
        .push("KSampler".into());
    let missing: WorkflowValidationResponse = api.post_json(path, &json!({"preset":"fast"})).await;
    assert!(!missing.valid);
    assert_eq!(missing.node_check.status, WorkflowNodeCheckStatus::Missing);
    assert_eq!(missing.node_check.missing_classes, ["KSampler"]);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    ApiHarness::assert_problem(
        api.request(Method::POST, path)
            .json(&json!({"preset":"unknown"}))
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    ApiHarness::assert_problem(
        api.request(Method::POST, path)
            .header(header::CONTENT_TYPE, "application/json")
            .body("invalid")
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn unavailable_and_malformed_backend_are_not_valid() {
    let api = ApiHarness::start().await;
    install(&api, None);
    let path = "/api/v1/workflows/contract-workflow/validate";
    api.fake
        .object_info_unavailable
        .store(true, Ordering::SeqCst);
    let result: WorkflowValidationResponse = api.post_json(path, &json!({"preset":"fast"})).await;
    assert!(!result.valid);
    assert_eq!(
        result.node_check.status,
        WorkflowNodeCheckStatus::Unavailable
    );
    assert!(result.node_check.reason.is_some());
    api.fake
        .object_info_unavailable
        .store(false, Ordering::SeqCst);
    *api.fake.object_info_override.lock().unwrap() = Some(json!([]));
    let result: WorkflowValidationResponse = api.post_json(path, &json!({"preset":"fast"})).await;
    assert!(!result.valid);
    assert_eq!(
        result.node_check.status,
        WorkflowNodeCheckStatus::Unavailable
    );
    *api.fake.object_info_override.lock().unwrap() = None;
    api.fake.object_info_hangs.store(true, Ordering::SeqCst);
    let result: WorkflowValidationResponse = api.post_json(path, &json!({"preset":"fast"})).await;
    assert!(!result.valid);
    assert_eq!(
        result.node_check.status,
        WorkflowNodeCheckStatus::Unavailable
    );
    assert!(result.node_check.reason.unwrap().contains("timed out"));
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn thumbnail_requires_a_safe_supported_local_file() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    std::fs::write(dir.join("image.png"), b"png bytes").unwrap();
    let path = "/api/v1/workflows/contract-workflow/thumbnail";
    let response = api.request(Method::GET, path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(response.bytes().await.unwrap(), &b"png bytes"[..]);
    for target in ["../outside.png", "/etc/passwd", "missing.png", "unsafe.svg"] {
        install(&api, Some(target));
        ApiHarness::assert_problem(
            api.request(Method::GET, path).send().await.unwrap(),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    std::fs::write(api.workspace().join("outside.png"), b"outside").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(api.workspace().join("outside.png"), dir.join("link.png"))
            .unwrap();
        install(&api, Some("link.png"));
        ApiHarness::assert_problem(
            api.request(Method::GET, path).send().await.unwrap(),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    std::fs::write(dir.join("unsafe.svg"), b"<svg></svg>").unwrap();
    install(&api, Some("unsafe.svg"));
    ApiHarness::assert_problem(
        api.request(Method::GET, path).send().await.unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    std::fs::write(dir.join("huge.png"), vec![0; 10 * 1024 * 1024 + 1]).unwrap();
    install(&api, Some("huge.png"));
    ApiHarness::assert_problem(
        api.request(Method::GET, path).send().await.unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    install(&api, None);
    ApiHarness::assert_problem(
        api.request(Method::GET, path).send().await.unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn unknown_ids_are_not_found_and_paths_are_not_accepted_as_ids() {
    let api = ApiHarness::start().await;
    install(&api, None);
    for (method, path) in [
        (Method::GET, "/api/v1/workflows/missing"),
        (Method::GET, "/api/v1/workflows/missing/thumbnail"),
        (Method::POST, "/api/v1/workflows/missing/validate"),
        (Method::GET, "/api/v1/workflows/contract.genaimage.json"),
    ] {
        let response = api
            .request(method, path)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
        assert!(problem.recovery.contains("workflow"));
    }
    api.shutdown().await;
}

#[tokio::test]
async fn defaults_and_ambiguous_or_invalid_identifiers_are_handled() {
    let api = ApiHarness::start().await;
    install(&api, None);
    let dir = &api.core.config().workflows;
    let manifest_path = dir.join("contract.genaimage.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["parameters"]["seed"]["default"] = json!(42);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let result: WorkflowValidationResponse = api
        .post_json(
            "/api/v1/workflows/contract-workflow/validate",
            &json!({"parameters":{"steps":10}}),
        )
        .await;
    assert!(result.valid);
    std::fs::write(
        dir.join("duplicate.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let response = api
        .request(Method::GET, "/api/v1/workflows/contract-workflow")
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("ambiguous"));
    manifest["schema_version"] = json!(1);
    std::fs::write(
        dir.join("duplicate.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let response = api
        .request(Method::GET, "/api/v1/workflows/contract-workflow")
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("invalid manifest"));
    api.shutdown().await;
}

#[tokio::test]
async fn catalogue_thumbnail_url_encodes_workflow_ids_as_one_path_segment() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    let path = dir.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["id"] = json!("workflow / one");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(dir.join("image.png"), b"image").unwrap();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    let url = list.workflows[0].thumbnail_url.as_deref().unwrap();
    let response = api.request(Method::GET, url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap(), &b"image"[..]);
    let details: WorkflowDetails = api.get_json(url.strip_suffix("/thumbnail").unwrap()).await;
    assert_eq!(details.id, "workflow / one");
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn thumbnail_fifos_and_symlinked_directories_are_rejected_promptly() {
    use rustix::fs::{mkfifoat, Mode, CWD};
    use std::time::Duration;

    let api = ApiHarness::start().await;
    install(&api, Some("pipe.png"));
    let dir = &api.core.config().workflows;
    mkfifoat(CWD, dir.join("pipe.png"), Mode::RUSR | Mode::WUSR).unwrap();
    let path = "/api/v1/workflows/contract-workflow/thumbnail";
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        api.request(Method::GET, path).send(),
    )
    .await
    .expect("FIFO open blocked")
    .unwrap();
    ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;

    let outside = api.workspace().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("image.png"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
    install(&api, Some("linked/image.png"));
    ApiHarness::assert_problem(
        api.request(Method::GET, path).send().await.unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;

    std::fs::create_dir(dir.join("nested")).unwrap();
    std::fs::write(dir.join("nested/image.png"), b"inside").unwrap();
    install(&api, Some("nested/image.png"));
    let response = api.request(Method::GET, path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap(), &b"inside"[..]);
    api.shutdown().await;
}

#[tokio::test]
async fn dot_segment_ids_are_discovery_diagnostics() {
    let api = ApiHarness::start().await;
    for id in [".", ".."] {
        install(&api, Some("image.png"));
        let path = api.core.config().workflows.join("contract.genaimage.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["id"] = json!(id);
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
        assert!(list.workflows.is_empty());
        assert_eq!(list.diagnostics.len(), 1);
        assert!(list.diagnostics[0].error.contains("choose a different ID"));
    }
    api.shutdown().await;
}

#[tokio::test]
async fn catalogue_indexes_thumbnails_and_invalidates_on_changes() {
    let api = ApiHarness::start().await;
    install(&api, Some("image.png"));
    let dir = &api.core.config().workflows;
    let path = dir.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::copy(dir.join("workflow.api.json"), dir.join("sibling.api.json")).unwrap();
    for i in 0..100 {
        manifest["id"] = json!(format!("workflow-{i}"));
        manifest["workflow"] = json!(if i == 99 {
            "sibling.api.json"
        } else {
            "workflow.api.json"
        });
        std::fs::write(
            dir.join(format!("workflow-{i}.genaimage.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(dir.join("image.png"), b"first").unwrap();
    let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(list.workflows.len(), 100);
    for item in &list.workflows {
        let response = api
            .request(Method::GET, item.thumbnail_url.as_deref().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap(), &b"first"[..]);
    }
    std::fs::write(dir.join("image.png"), b"updated").unwrap();
    let changed = api
        .request(Method::GET, "/api/v1/workflows/workflow-0/thumbnail")
        .send()
        .await
        .unwrap();
    assert_eq!(changed.bytes().await.unwrap(), &b"updated"[..]);
    #[cfg(unix)]
    {
        use rustix::fs::{mkfifoat, Mode, CWD};
        use std::time::Duration;

        let sibling = dir.join("sibling.api.json");
        std::fs::remove_file(&sibling).unwrap();
        mkfifoat(CWD, &sibling, Mode::RUSR | Mode::WUSR).unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            api.request(Method::GET, "/api/v1/workflows/workflow-0/thumbnail")
                .send(),
        )
        .await;
        if response.is_err() {
            let unblock = sibling.clone();
            tokio::task::spawn_blocking(move || std::fs::write(unblock, b"{}"));
        }
        assert_eq!(
            response
                .expect("workflow discovery blocked on another workflow's FIFO")
                .unwrap()
                .status(),
            StatusCode::OK
        );
        std::fs::remove_file(&sibling).unwrap();
        std::fs::copy(dir.join("workflow.api.json"), sibling).unwrap();
    }
    let target = dir.join("workflow-0.genaimage.json");
    manifest["workflow"] = json!("workflow.api.json");
    manifest["id"] = json!("workflow-0");
    manifest["thumbnail"] = json!("changed.png");
    std::fs::write(&target, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(dir.join("changed.png"), b"changed").unwrap();
    let url = "/api/v1/workflows/workflow-0/thumbnail";
    assert_eq!(
        api.request(Method::GET, url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        &b"changed"[..]
    );
    std::fs::write(
        dir.join("duplicate.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let response = api
        .request(Method::GET, "/api/v1/workflows/workflow-0")
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("ambiguous"));
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let response = api.request(Method::GET, url).send().await.unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("ambiguous"));
    let mut invalid = manifest.clone();
    invalid["schema_version"] = json!(1);
    std::fs::write(
        dir.join("duplicate.genaimage.json"),
        serde_json::to_vec(&invalid).unwrap(),
    )
    .unwrap();
    let _: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    let response = api.request(Method::GET, url).send().await.unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("invalid manifest"));
    std::fs::remove_file(dir.join("duplicate.genaimage.json")).unwrap();
    let _: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(
        api.request(Method::GET, url).send().await.unwrap().status(),
        StatusCode::OK
    );
    let sibling = dir.join("workflow-1.genaimage.json");
    let original = std::fs::read(&sibling).unwrap();
    std::fs::write(&sibling, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let _: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    let response = api.request(Method::GET, url).send().await.unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(problem.detail.contains("ambiguous"));
    std::fs::write(&sibling, original).unwrap();
    let _: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(
        api.request(Method::GET, url).send().await.unwrap().status(),
        StatusCode::OK
    );
    std::fs::remove_file(&target).unwrap();
    ApiHarness::assert_problem(
        api.request(Method::GET, url).send().await.unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn malformed_paths_queries_and_json_return_problem_details() {
    let api = ApiHarness::start().await;
    for (method, path, body) in [
        (Method::GET, "/api/v1/workflows/%FF", None),
        (Method::POST, "/api/v1/workflows/%FF/validate", Some("{}")),
        (Method::GET, "/api/v1/workflows/%FF/thumbnail", None),
        (Method::GET, "/api/v1/runs/%FF", None),
        (Method::GET, "/api/v1/outputs/%FF", None),
        (Method::GET, "/api/v1/uploads/%FF", None),
        (Method::GET, "/api/v1/outputs?limit=invalid", None),
        (Method::POST, "/api/v1/settings/test", Some("{")),
    ] {
        let mut request = api.request(method, path);
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        ApiHarness::assert_problem(request.send().await.unwrap(), StatusCode::BAD_REQUEST).await;
    }
}
