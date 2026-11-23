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
    #[cfg(unix)]
    {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap(), &b"image"[..]);
    }
    #[cfg(not(unix))]
    ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
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
        install(&api, None);
        let path = api.core.config().workflows.join("contract.genaimage.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["id"] = json!(id);
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let list: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
        assert!(list.workflows.is_empty());
        assert_eq!(list.diagnostics.len(), 1);
        assert!(list.diagnostics[0].error.contains("workflow id cannot"));
    }
    api.shutdown().await;
}
