mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::ApiHarness;
use genaimage_api::{
    client::{Client, Error},
    SuggestWorkflowRequest, SuggestWorkflowResponse,
};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer};
use reqwest::StatusCode;
use serde_json::{json, Value};

async fn configured(response: &str) -> (ApiHarness, FakeLlmServer) {
    configured_script(FakeLlmScript::text_chunks([response])).await
}

async fn configured_script(script: FakeLlmScript) -> (ApiHarness, FakeLlmServer) {
    let script = script.with_models_response(json!({
        "object": "list",
        "data": [{
            "id": "assistant",
            "supports_tools": true,
            "supports_vision": false
        }]
    }));
    let llm = FakeLlmServer::start(script).await;
    let base_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", &base_url),
            ("GENAIMAGE_LLM_TEXT_MODEL", "assistant"),
        ],
    )
    .await;
    (api, llm)
}

fn client(api: &ApiHarness) -> Client {
    Client::with_base_url(&api.origin).unwrap()
}

fn install_workflows(api: &ApiHarness, ids: &[&str]) {
    let root = api.core.config().workflows.clone();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("suggest.api.json"),
        br#"{"3":{"class_type":"KSampler","inputs":{"seed":1,"steps":20,"strength":1.0,"reference":""}},"9":{"class_type":"SaveImage","inputs":{"filename_prefix":"genaimage"}}}"#,
    )
    .unwrap();
    for id in ids {
        let manifest = json!({
            "schema_version": 2,
            "id": id,
            "version": "1.0.0",
            "title": format!("Workflow {id}"),
            "workflow": "suggest.api.json",
            "description": format!("Description for {id}"),
            "tags": ["test", id],
            "parameters": {
                "seed": {
                    "node": "3", "input": "seed", "kind": "seed", "required": true,
                    "description": "Exact seed", "widget": "number", "step": 1
                },
                "steps": {
                    "node": "3", "input": "steps", "kind": "integer", "required": true,
                    "minimum": 1, "maximum": 100, "description": "Sampling steps"
                },
                "strength": {
                    "node": "3", "input": "strength", "kind": "number", "required": true,
                    "minimum": 0.0, "maximum": 2.0, "description": "Strength"
                },
                "reference_image": {
                    "node": "3", "input": "reference", "kind": "image_upload", "required": true,
                    "description": "User-selected reference image", "widget": "file"
                }
            },
            "presets": {
                "fast": {"seed": 42, "steps": 10, "strength": 1.0}
            },
            "outputs": [{"node": "9", "kind": "image"}]
        });
        std::fs::write(
            root.join(format!("{id}.genaimage.json")),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }
}

async fn suggest(
    client: &Client,
    intent: &str,
    limit: Option<usize>,
) -> Result<SuggestWorkflowResponse, Error> {
    client
        .suggest_workflows(&SuggestWorkflowRequest {
            intent: intent.to_owned(),
            limit,
        })
        .await
}

fn llm_response(suggestions: Value) -> String {
    json!({"suggestions": suggestions}).to_string()
}

#[tokio::test]
async fn preserves_llm_ranking_renumbers_and_deduplicates() {
    let response = llm_response(json!([
        {"rank": 20, "workflow_id": "third", "preset": "fast", "parameters": {}, "rationale": "third"},
        {"rank": 10, "workflow_id": "first", "preset": "fast", "parameters": {}, "rationale": "first"},
        {"rank": 15, "workflow_id": "missing", "preset": "fast", "parameters": {}, "rationale": "missing"},
        {"rank": 20, "workflow_id": "first", "preset": "fast", "parameters": {}, "rationale": "duplicate"},
        {"rank": 30, "workflow_id": "second", "preset": "fast", "parameters": {}, "rationale": "second"}
    ]));
    let (api, _llm) = configured(&response).await;
    install_workflows(&api, &["first", "second", "third"]);

    let response = suggest(&client(&api), "rank workflows", Some(5))
        .await
        .unwrap();
    assert_eq!(
        response
            .suggestions
            .iter()
            .map(|item| item.workflow_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "third", "second"]
    );
    assert_eq!(
        response
            .suggestions
            .iter()
            .map(|item| item.rank)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(response
        .dropped
        .iter()
        .any(|item| item.workflow_id == "missing" && item.field == "workflow_id"));
    assert!(response
        .dropped
        .iter()
        .any(|item| item.workflow_id == "first" && item.reason.contains("duplicate")));
    assert_eq!(response.applied, genaimage_api::AppliedFalse);
    assert!(api.core.list_runs(10).unwrap().is_empty());
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn drops_unknown_workflow_preset_parameter_and_invalid_value_without_correction() {
    let response = llm_response(json!([
        {"rank": 1, "workflow_id": "absent", "preset": "fast", "parameters": {}, "rationale": "unknown workflow"},
        {"rank": 2, "workflow_id": "known", "preset": "unknown", "parameters": {}, "rationale": "unknown preset"},
        {"rank": 3, "workflow_id": "known", "preset": "fast", "parameters": {"invented": 4}, "rationale": "unknown parameter"},
        {"rank": 4, "workflow_id": "known", "preset": "fast", "parameters": {"steps": 0}, "rationale": "invalid value"}
    ]));
    let (api, _llm) = configured(&response).await;
    install_workflows(&api, &["known"]);

    let response = suggest(&client(&api), "validate everything", None)
        .await
        .unwrap();
    assert!(response.suggestions.is_empty());
    assert!(response
        .dropped
        .iter()
        .any(|item| item.workflow_id == "absent" && item.field == "workflow_id"));
    assert!(response
        .dropped
        .iter()
        .any(|item| item.workflow_id == "known" && item.field == "preset"));
    assert!(response
        .dropped
        .iter()
        .any(|item| item.field == "parameters.invented"));
    assert!(response
        .dropped
        .iter()
        .any(|item| item.field == "parameters.steps" && item.reason.contains("minimum")));
    assert!(api.core.list_runs(10).unwrap().is_empty());
}

#[tokio::test]
async fn removes_image_inputs_without_dropping_an_otherwise_valid_suggestion() {
    let response = llm_response(json!([{
        "rank": 1,
        "workflow_id": "images",
        "preset": "fast",
        "parameters": {"reference_image": "not-an-upload-id", "steps": 12},
        "rationale": "retain this proposal"
    }]));
    let (api, llm) = configured(&response).await;
    install_workflows(&api, &["images"]);

    let response = suggest(&client(&api), "use a reference image", Some(1))
        .await
        .unwrap();
    assert_eq!(response.suggestions.len(), 1);
    assert!(!response.suggestions[0]
        .parameters
        .contains_key("reference_image"));
    assert_eq!(response.suggestions[0].parameters["steps"], json!(12));
    assert!(response.dropped.iter().any(|item| {
        item.field == "parameters.reference_image"
            && item.reason == "image inputs must be chosen by the user"
    }));
    let request = &llm.recorded_requests()[0];
    let prompt = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(!prompt.contains("reference_image"));
}

#[tokio::test]
async fn proposal_validation_rejects_image_values_by_default_and_harness_checks_uploads() {
    let (api, _llm) = configured(&llm_response(json!([]))).await;
    install_workflows(&api, &["images"]);
    let workflow = api.core.load_workflow_by_id("images").unwrap().unwrap();
    let parameters = std::collections::BTreeMap::from([(
        "reference_image".to_owned(),
        json!("sha256:not-stored"),
    )]);

    let issues =
        genaimage_core::assistant::suggest::validate_proposal(&workflow, Some("fast"), &parameters)
            .unwrap_err();
    assert!(issues.iter().any(|issue| {
        issue.field == "parameters.reference_image"
            && issue.reason == "image inputs must be chosen by the user"
    }));

    let missing = std::collections::BTreeMap::new();
    let issues = api
        .core
        .validate_proposal_with_uploads(&workflow, Some("fast"), &missing)
        .await
        .unwrap_err();
    assert!(issues.iter().any(|issue| {
        issue.field == "parameters.reference_image" && issue.reason.contains("no value")
    }));

    let issues = api
        .core
        .validate_proposal_with_uploads(&workflow, Some("fast"), &parameters)
        .await
        .unwrap_err();
    assert!(issues.iter().any(|issue| {
        issue.field == "parameters.reference_image" && issue.reason.contains("Unknown upload ID")
    }));
}

#[tokio::test]
async fn excludes_and_rejects_presets_that_supply_image_uploads() {
    let response = llm_response(json!([{
        "rank": 1,
        "workflow_id": "images",
        "preset": "with_image",
        "parameters": {},
        "rationale": "uses image preset"
    }]));
    let (api, llm) = configured(&response).await;
    install_workflows(&api, &["images"]);
    let path = api.core.config().workflows.join("images.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["presets"]["with_image"] = json!({
        "seed": 42,
        "steps": 10,
        "strength": 1.0,
        "reference_image": "sha256:not-shown"
    });
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let workflow = api.core.load_workflow_by_id("images").unwrap().unwrap();
    let issues = genaimage_core::assistant::suggest::validate_proposal(
        &workflow,
        Some("with_image"),
        &std::collections::BTreeMap::new(),
    )
    .unwrap_err();
    assert!(issues.iter().any(|issue| {
        issue.field == "preset"
            && issue.reason == "preset binds an image input the user must choose"
    }));

    let response = suggest(&client(&api), "use the image preset", Some(1))
        .await
        .unwrap();
    assert!(response.suggestions.is_empty());
    assert!(response.dropped.iter().any(|item| {
        item.field == "preset" && item.reason == "preset binds an image input the user must choose"
    }));
    let request = &llm.recorded_requests()[0];
    let prompt = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(!prompt.contains("with_image"));
}

#[tokio::test]
async fn drops_a_workflow_modified_after_the_catalogue_snapshot() {
    let response = llm_response(json!([{
        "rank": 1,
        "workflow_id": "changing",
        "preset": "fast",
        "parameters": {},
        "rationale": "catalogue snapshot"
    }]));
    let script = FakeLlmScript::text_chunks([&response]).with_events([
        FakeLlmEvent::Delay(Duration::from_millis(250)),
        FakeLlmEvent::text(response),
        FakeLlmEvent::finish("stop"),
        FakeLlmEvent::Done,
    ]);
    let (api, llm) = configured_script(script).await;
    install_workflows(&api, &["changing"]);
    let api_client = client(&api);
    let request =
        tokio::spawn(
            async move { suggest(&api_client, "use the reviewed workflow", Some(1)).await },
        );
    assert!(llm.wait_for_chat_requests(1, Duration::from_secs(2)).await);
    let path = api.core.config().workflows.join("changing.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["description"] = json!("Changed after the catalogue was sent");
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let response = request.await.unwrap().unwrap();
    assert!(response.suggestions.is_empty());
    assert!(response.dropped.iter().any(|item| {
        item.workflow_id == "changing"
            && item.field == "workflow_id"
            && item.reason == "workflow changed during suggestion"
    }));
}

#[tokio::test]
async fn returns_exact_numbers_and_the_validated_revision() {
    let response = r#"{"suggestions":[{"rank":1,"workflow_id":"precise","preset":"fast","parameters":{"seed":18446744073709551615,"strength":2.0},"rationale":"keep exact values","parameter_notes":{"seed":"maximum seed"}}]}"#;
    let (api, _llm) = configured(response).await;
    install_workflows(&api, &["precise"]);
    let workflow = api.core.load_workflow_by_id("precise").unwrap().unwrap();

    let response = suggest(&client(&api), "exact numbers", Some(1))
        .await
        .unwrap();
    let suggestion = &response.suggestions[0];
    assert_eq!(suggestion.workflow_revision, workflow.revision());
    assert_eq!(
        suggestion.parameters["seed"].to_string(),
        "18446744073709551615"
    );
    assert_eq!(suggestion.parameters["strength"].to_string(), "2.0");
    assert_eq!(suggestion.parameter_notes["seed"], "maximum seed");
    assert_eq!(response.model, "assistant");
    assert_eq!(response.template.name, "suggest");
    assert_eq!(response.template.sha256.len(), 64);
    assert_eq!(response.applied, genaimage_api::AppliedFalse);
}

#[tokio::test]
async fn reports_catalogue_truncation_and_bounds_the_prompt() {
    let omitted_response = llm_response(json!([{
        "rank": 1,
        "workflow_id": "workflow-49",
        "preset": "fast",
        "parameters": {},
        "rationale": "omitted workflow"
    }]));
    let (api, llm) = configured(&omitted_response).await;
    let ids = (0..50)
        .map(|index| format!("workflow-{index:02}"))
        .collect::<Vec<_>>();
    let borrowed = ids.iter().map(String::as_str).collect::<Vec<_>>();
    install_workflows(&api, &borrowed);
    let root = api.core.config().workflows.clone();
    for id in ids {
        let path = root.join(format!("{id}.genaimage.json"));
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["description"] = json!("x".repeat(1800));
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }

    let response = suggest(&client(&api), "large catalogue", None)
        .await
        .unwrap();
    assert!(response.truncated);
    let request = &llm.recorded_requests()[0];
    let prompt = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("Catalogue truncated: true"));
    assert!(prompt.contains(GENAIMAGE_CATALOGUE_MARKER));
    assert!(!prompt.contains("workflow-49"));
    assert!(prompt.len() <= 70 * 1024);
    assert!(response.suggestions.is_empty());
    assert!(response.dropped.iter().any(|item| {
        item.workflow_id == "workflow-49"
            && item.field == "workflow_id"
            && item.reason.contains("not in the catalogue snapshot")
    }));
}

const GENAIMAGE_CATALOGUE_MARKER: &str = "Workflow catalogue JSON:";

#[tokio::test]
async fn malformed_llm_json_is_a_bad_gateway() {
    let (api, _llm) = configured("not json").await;
    install_workflows(&api, &["known"]);
    let error = suggest(&client(&api), "broken provider", None)
        .await
        .unwrap_err();
    let Error::Problem(problem) = error else {
        panic!("expected problem details")
    };
    assert_eq!(problem.status, StatusCode::BAD_GATEWAY.as_u16());
    assert!(problem.detail.contains("malformed"));
    assert!(api.core.list_runs(10).unwrap().is_empty());
}

#[tokio::test]
async fn oversized_intents_are_rejected_before_provider_access() {
    let (api, llm) = configured(&llm_response(json!([]))).await;
    let client = client(&api);

    for (intent, status) in [
        ("a".repeat(4001), StatusCode::UNPROCESSABLE_ENTITY),
        ("💧".repeat(5000), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let error = suggest(&client, &intent, None).await.unwrap_err();
        let Error::Problem(problem) = error else {
            panic!("expected problem details")
        };
        assert_eq!(problem.status, status.as_u16());
    }
    assert_eq!(llm.models_request_count(), 0);
    assert_eq!(llm.chat_request_count(), 0);
}

#[tokio::test]
async fn unconfigured_text_role_is_unavailable_without_calling_any_backend() {
    let api = ApiHarness::start().await;
    install_workflows(&api, &["known"]);
    let error = suggest(&client(&api), "not configured", None)
        .await
        .unwrap_err();
    let Error::Problem(problem) = error else {
        panic!("expected problem details")
    };
    assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE.as_u16());
    assert!(api.core.list_runs(10).unwrap().is_empty());
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
}
