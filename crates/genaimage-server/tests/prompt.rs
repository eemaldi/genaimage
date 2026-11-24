mod common;

use std::fs;

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    client::{Client, Error},
    PromptAssistMode, PromptAssistRequest, PromptDiffOperation,
};
use genaimage_test_support::{FakeLlmScript, FakeLlmServer};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

async fn configured(script: FakeLlmScript) -> (ApiHarness, FakeLlmServer) {
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

fn request(mode: PromptAssistMode, prompt: &str) -> PromptAssistRequest {
    PromptAssistRequest {
        mode,
        prompt: prompt.into(),
        instruction: None,
        n: None,
        original: None,
        workflow_id: None,
        parameter: None,
    }
}

fn assert_problem(error: Error, status: StatusCode) -> genaimage_api::ProblemDetails {
    match error {
        Error::Problem(problem) => {
            assert_eq!(problem.status, status.as_u16());
            *problem
        }
        other => panic!("expected problem details, got {other}"),
    }
}

#[tokio::test]
async fn returns_proposals_for_each_mode_with_server_computed_diffs() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([
        r#"{"proposals":["expanded prompt"]}"#,
    ]))
    .await;
    let client = client(&api);

    let expand = client
        .prompt_assist(&request(PromptAssistMode::Expand, "short prompt"))
        .await
        .unwrap();
    assert_eq!(expand.mode, PromptAssistMode::Expand);
    assert_eq!(expand.proposals[0].text, "expanded prompt");
    assert!(!expand.applied);
    assert_eq!(expand.proposal_count, 1);

    llm.set_script(FakeLlmScript::text_chunks([
        r#"{"proposals":["refined prompt"]}"#,
    ]));
    let mut refine = request(PromptAssistMode::Refine, "short prompt");
    refine.instruction = Some("make it cinematic".into());
    assert_eq!(
        client.prompt_assist(&refine).await.unwrap().proposals[0].text,
        "refined prompt"
    );

    llm.set_script(FakeLlmScript::text_chunks([
        r#"{"proposals":["blur, watermark"]}"#,
    ]));
    let mut negative = request(PromptAssistMode::Negative, "portrait");
    negative.original = Some("blur".into());
    let negative = client.prompt_assist(&negative).await.unwrap();
    assert_eq!(negative.proposals[0].text, "blur, watermark");
    assert_eq!(negative.proposals[0].diff[0].op, PromptDiffOperation::Equal);
    assert!(negative.proposals[0]
        .diff
        .iter()
        .any(|part| part.op == PromptDiffOperation::Insert));

    llm.set_script(FakeLlmScript::text_chunks([
        r#"{"proposals":["one","two","three","four"]}"#,
    ]));
    let mut variations = request(PromptAssistMode::Variations, "base");
    variations.n = Some(4);
    let variations = client.prompt_assist(&variations).await.unwrap();
    assert_eq!(variations.proposals.len(), 4);
    assert_eq!(variations.proposal_count, 4);
    for template in [
        "prompt_expand.md",
        "prompt_refine.md",
        "prompt_negative.md",
        "prompt_variations.md",
    ] {
        assert!(api
            .core
            .config()
            .data_dir()
            .join("assistant/prompts")
            .join(template)
            .is_file());
    }
}

#[tokio::test]
async fn workflow_description_tags_and_target_parameter_hints_reach_the_llm() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([
        r#"{"proposals":["proposal"]}"#,
    ]))
    .await;
    install_workflow(&api);
    let manifest_path = api.core.config().workflows.join("contract.genaimage.json");
    let graph_path = api.core.config().workflows.join("workflow.api.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["description"] = json!("A precise portrait workflow");
    manifest["tags"] = json!(["portrait", "studio"]);
    manifest["parameters"]["prompt"] = json!({
        "node": "11", "input": "text", "kind": "string", "required": true,
        "description": "Main portrait direction", "widget": "textarea",
        "placeholder": "Describe the subject"
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut graph: Value = serde_json::from_slice(&fs::read(&graph_path).unwrap()).unwrap();
    graph["11"] = json!({"class_type":"CLIPTextEncode","inputs":{"text":""}});
    fs::write(&graph_path, serde_json::to_vec(&graph).unwrap()).unwrap();

    let mut request = request(PromptAssistMode::Expand, "portrait");
    request.workflow_id = Some("contract-workflow".into());
    request.parameter = Some("prompt".into());
    client(&api).prompt_assist(&request).await.unwrap();

    let recorded = llm.recorded_requests();
    let user = recorded[0]["messages"][1]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(user.contains("A precise portrait workflow"));
    assert!(user.contains("studio"));
    assert!(user.contains("Main portrait direction"));
    assert!(user.contains("textarea"));
    assert!(user.contains("Describe the subject"));
}

#[tokio::test]
async fn refine_requires_an_instruction_before_calling_the_llm() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([r#"{"proposals":["ok"]}"#])).await;
    let error = client(&api)
        .prompt_assist(&request(PromptAssistMode::Refine, "prompt"))
        .await
        .err()
        .unwrap();
    let problem = assert_problem(error, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.detail.contains("instruction is required"));
    assert_eq!(llm.chat_request_count(), 0);
}

#[tokio::test]
async fn variations_are_bounded_and_empty_or_duplicate_values_are_not_padded() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([
        r#"{"proposals":["one","","one","two"]}"#,
    ]))
    .await;
    for n in [0, 9] {
        let mut invalid = request(PromptAssistMode::Variations, "base");
        invalid.n = Some(n);
        assert_problem(
            client(&api).prompt_assist(&invalid).await.unwrap_err(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    assert_eq!(llm.chat_request_count(), 0);
    let mut valid = request(PromptAssistMode::Variations, "base");
    valid.n = Some(4);
    let response = client(&api).prompt_assist(&valid).await.unwrap();
    assert_eq!(response.proposals.len(), 2);
    assert_eq!(response.proposal_count, 2);
}

#[tokio::test]
async fn malformed_json_is_a_bad_gateway_without_fabricated_proposals() {
    let (api, _llm) = configured(FakeLlmScript::text_chunks(["not JSON"])).await;
    let problem = assert_problem(
        client(&api)
            .prompt_assist(&request(PromptAssistMode::Expand, "base"))
            .await
            .unwrap_err(),
        StatusCode::BAD_GATEWAY,
    );
    assert!(problem.detail.contains("malformed strict JSON"));
}

#[tokio::test]
async fn oversized_llm_response_is_a_bad_gateway() {
    let (api, _llm) = configured(FakeLlmScript::text_chunks(["x".repeat(64 * 1024 + 1)])).await;
    assert_problem(
        client(&api)
            .prompt_assist(&request(PromptAssistMode::Expand, "base"))
            .await
            .unwrap_err(),
        StatusCode::BAD_GATEWAY,
    );
}

#[tokio::test]
async fn prompt_assist_reports_not_configured_and_unknown_workflow() {
    let api = ApiHarness::start().await;
    let not_configured = assert_problem(
        client(&api)
            .prompt_assist(&request(PromptAssistMode::Expand, "base"))
            .await
            .unwrap_err(),
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(not_configured.detail.contains("not configured"));

    let mut unknown = request(PromptAssistMode::Expand, "base");
    unknown.workflow_id = Some("missing".into());
    assert_problem(
        client(&api).prompt_assist(&unknown).await.unwrap_err(),
        StatusCode::NOT_FOUND,
    );
}

#[tokio::test]
async fn prompt_and_instruction_limits_are_rejected_as_unprocessable() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([r#"{"proposals":["ok"]}"#])).await;
    let mut too_long = request(PromptAssistMode::Expand, &"x".repeat(16 * 1024 + 1));
    assert_problem(
        client(&api).prompt_assist(&too_long).await.unwrap_err(),
        StatusCode::UNPROCESSABLE_ENTITY,
    );
    too_long = request(PromptAssistMode::Refine, "base");
    too_long.instruction = Some("x".repeat(8 * 1024 + 1));
    assert_problem(
        client(&api).prompt_assist(&too_long).await.unwrap_err(),
        StatusCode::UNPROCESSABLE_ENTITY,
    );
    assert_eq!(llm.chat_request_count(), 0);
}

#[tokio::test]
async fn maximum_length_four_byte_refine_request_reaches_validation() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([r#"{"proposals":["refined"]}"#])).await;
    let mut body = request(PromptAssistMode::Refine, &"🦀".repeat(16 * 1024));
    body.instruction = Some("🦀".repeat(8 * 1024));
    let response = client(&api).prompt_assist(&body).await.unwrap();
    assert_eq!(response.proposals[0].text, "refined");
    assert_eq!(llm.chat_request_count(), 1);
}

#[tokio::test]
async fn edited_mode_template_is_used_and_its_hash_is_recorded() {
    let (api, llm) = configured(FakeLlmScript::text_chunks([r#"{"proposals":["first"]}"#])).await;
    let client = client(&api);
    let before = client
        .prompt_assist(&request(PromptAssistMode::Expand, "base"))
        .await
        .unwrap();
    assert_eq!(before.template.name, "prompt_expand");

    let edited = "USER EDIT: preserve this instruction and return strict JSON.\n";
    let path = api
        .core
        .config()
        .data_dir()
        .join("assistant/prompts/prompt_expand.md");
    fs::write(path, edited).unwrap();
    llm.set_script(FakeLlmScript::text_chunks([r#"{"proposals":["second"]}"#]));
    let after = client
        .prompt_assist(&request(PromptAssistMode::Expand, "base"))
        .await
        .unwrap();
    assert_ne!(before.template.sha256, after.template.sha256);
    assert_eq!(
        after.template.sha256,
        hex::encode(Sha256::digest(edited.as_bytes()))
    );
    let recorded = llm.recorded_requests();
    assert_eq!(recorded[1]["messages"][0]["content"], edited);
}
