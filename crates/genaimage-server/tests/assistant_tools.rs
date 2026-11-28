mod common;

use std::{io::Cursor, sync::atomic::Ordering};

use common::{install_workflow, ApiHarness};
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantStreamEvent, Client, Error},
    AssistantChatContent, AssistantConfirmationDecisionKind, AssistantConfirmationRequest,
    AssistantToolConfirmationStatus, CreateAssistantConversationRequest,
    SendAssistantMessageRequest,
};
use genaimage_core::{LlmRoles, RunRequest, RunStatus, ToolContext};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::StatusCode;
use serde_json::{json, Value};

const TEXT_MODEL: &str = "assistant-text";
const VISION_MODEL: &str = "assistant-vision";

fn models_script(events: Vec<FakeLlmEvent>, vision: bool) -> FakeLlmScript {
    FakeLlmScript::default()
        .with_events(events)
        .with_models_response(json!({
            "object": "list",
            "data": [
                {"id": TEXT_MODEL, "supports_tools": true, "supports_vision": false},
                {"id": VISION_MODEL, "supports_tools": false, "supports_vision": vision}
            ]
        }))
}

fn tool_script(id: &str, name: &str, arguments: Value) -> FakeLlmScript {
    models_script(
        vec![
            FakeLlmEvent::tool_calls([FakeToolCallDelta::new(0)
                .id(id)
                .name(name)
                .arguments(serde_json::to_string(&arguments).unwrap())]),
            FakeLlmEvent::finish("tool_calls"),
            FakeLlmEvent::Done,
        ],
        true,
    )
}

fn text_script(text: &str) -> FakeLlmScript {
    models_script(
        vec![
            FakeLlmEvent::text(text),
            FakeLlmEvent::finish("stop"),
            FakeLlmEvent::Done,
        ],
        true,
    )
}

async fn configured_with_builtin_tools() -> (ApiHarness, FakeLlmServer) {
    let llm = FakeLlmServer::start(text_script("ready")).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_builtin_assistant_tools(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", TEXT_MODEL),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
    )
    .await;
    (api, llm)
}

fn client(api: &ApiHarness) -> Client {
    Client::with_base_url(&api.origin).unwrap()
}

fn message(text: &str) -> SendAssistantMessageRequest {
    SendAssistantMessageRequest {
        content: vec![AssistantChatContent::Text(text.to_owned())],
    }
}

async fn create(client: &Client) -> genaimage_api::AssistantConversationResponse {
    client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Built-in tools".into(),
        })
        .await
        .unwrap()
}

async fn pause(
    stream: &mut genaimage_api::client::AssistantEventStream,
) -> genaimage_api::AssistantConfirmationRequiredEvent {
    let mut confirmation = None;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ConfirmationRequired(event) => confirmation = Some(event),
            AssistantStreamEvent::AwaitingConfirmation(_) => {
                return confirmation.expect("confirmation_required before awaiting")
            }
            AssistantStreamEvent::Error(event) => panic!("assistant failed: {event:?}"),
            _ => {}
        }
    }
    panic!("assistant stream ended before awaiting_confirmation")
}

async fn finish(stream: &mut genaimage_api::client::AssistantEventStream) -> Value {
    let mut result = None;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ToolResult(event) if event.ok => {
                result = event.result;
            }
            AssistantStreamEvent::Done(_) => return result.unwrap_or(Value::Null),
            AssistantStreamEvent::Error(event) => panic!("assistant failed: {event:?}"),
            _ => {}
        }
    }
    panic!("assistant stream ended without done")
}

fn install_exact_workflow(api: &ApiHarness) -> String {
    install_workflow(api);
    let manifest_path = api.core.config().workflows.join("contract.genaimage.json");
    let graph_path = api.core.config().workflows.join("workflow.api.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["parameters"]["ratio"] = json!({
        "node": "3", "input": "ratio", "kind": "number", "required": true
    });
    manifest["parameters"]["quality"] = json!({
        "node": "3", "input": "quality", "kind": "number", "required": false,
        "description": "Inherited graph quality"
    });
    manifest["parameters"]["source_image"] = json!({
        "node": "3", "input": "source_image", "kind": "image_upload", "required": false,
        "widget": "file"
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let mut graph: Value = serde_json::from_slice(&std::fs::read(&graph_path).unwrap()).unwrap();
    graph["3"]["inputs"]["ratio"] = json!(1.5);
    graph["3"]["inputs"]["quality"] = json!(0.75);
    graph["3"]["inputs"]["source_image"] = json!("existing.png");
    std::fs::write(&graph_path, serde_json::to_vec_pretty(&graph).unwrap()).unwrap();
    api.core
        .load_workflow_by_id("contract-workflow")
        .unwrap()
        .unwrap()
        .revision()
}

async fn pause_submit(
    llm: &FakeLlmServer,
    client: &Client,
    arguments: Value,
) -> (String, String, String, Value) {
    let conversation = create(client).await;
    llm.set_scripts([
        tool_script("submit", "submit_run", arguments),
        text_script("submitted"),
    ]);
    let mut stream = client
        .send_assistant_message(&conversation.id, &message("submit it"))
        .await
        .unwrap();
    let required = pause(&mut stream).await;
    assert_eq!(required.status, AssistantToolConfirmationStatus::Pending);
    (
        conversation.id,
        required.confirmation_id,
        required.preview_token,
        required.arguments,
    )
}

#[tokio::test]
async fn full_builtin_flow_previews_effective_values_and_reuses_confirmation_key() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    let conversation = create(&client).await;
    let raw_parameters = json!({"seed": 9007199254740993_u64, "ratio": 2.0});
    let arguments = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": revision,
        "preset": "fast",
        "parameters": raw_parameters
    });
    llm.set_scripts([
        tool_script("list", "list_workflows", json!({})),
        tool_script("propose", "propose_run", arguments.clone()),
        tool_script("submit", "submit_run", arguments),
        text_script("submitted"),
    ]);
    api.fake.complete.store(true, Ordering::SeqCst);
    let mut stream = client
        .send_assistant_message(&conversation.id, &message("make the reviewed run"))
        .await
        .unwrap();
    let required = pause(&mut stream).await;
    let confirmation_id = required.confirmation_id.clone();
    let preview_token = required.preview_token.clone();
    assert_eq!(
        required.arguments["parameters"]["seed"],
        json!(9007199254740993_u64)
    );
    assert_eq!(
        required.preview.as_ref().unwrap()["parameters"]["seed"],
        json!(9007199254740993_u64)
    );
    assert_eq!(
        required.preview.as_ref().unwrap()["parameters"]["ratio"].to_string(),
        "2.0"
    );
    assert_eq!(
        required.preview.as_ref().unwrap()["parameters"]["steps"],
        json!(10)
    );
    assert_eq!(
        required.preview.as_ref().unwrap()["parameters"]["quality"],
        json!(0.75)
    );
    assert_eq!(
        required.preview.as_ref().unwrap()["parameters"]["source_image"],
        json!("existing.png")
    );

    let mut confirmed = client
        .decide_assistant_confirmation(
            &conversation.id,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Confirm,
                arguments: None,
                reason: None,
                preview_token: Some(preview_token.clone()),
            },
        )
        .await
        .unwrap();
    let submitted = finish(&mut confirmed).await;
    let run_id = submitted["run_id"].as_str().unwrap();
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    let connection = rusqlite::Connection::open(api.core.config().state_database.clone()).unwrap();
    let mapping: (String, String) = connection
        .query_row("SELECT key, run_id FROM run_submission_keys", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(mapping, (confirmation_id, run_id.to_owned()));
    let run = api.core.get_run(run_id).unwrap();
    assert_eq!(run.parameters["seed"], json!(9007199254740993_u64));
    assert_eq!(run.parameters["ratio"].to_string(), "2.0");
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    assert_eq!(graph["3"]["inputs"]["seed"], json!(9007199254740993_u64));
    assert_eq!(graph["3"]["inputs"]["ratio"].to_string(), "2.0");

    llm.set_scripts([
        tool_script("status", "run_status", json!({"run_id": run_id})),
        text_script("completed"),
    ]);
    let mut status = client
        .send_assistant_message(&conversation.id, &message("show the run status"))
        .await
        .unwrap();
    let result = finish(&mut status).await;
    assert_eq!(result["run_id"], run_id);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["outputs"].as_array().unwrap().len(), 1);

    let audit = client
        .assistant_tool_calls(
            &conversation.id,
            &genaimage_api::AssistantToolAuditQuery::default(),
        )
        .await
        .unwrap();
    for name in ["list_workflows", "propose_run", "submit_run", "run_status"] {
        assert!(audit.tool_calls.iter().any(|call| call.tool == name));
    }
}

#[tokio::test]
async fn edit_uses_only_edited_values_and_reject_creates_no_run() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    let (conversation, confirmation_id, _preview_token, _) = pause_submit(
        &llm,
        &client,
        json!({
            "workflow_id": "contract-workflow",
            "workflow_revision": revision,
            "preset": "fast",
            "parameters": {"seed": 11, "ratio": 1.25}
        }),
    )
    .await;
    let edited = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": revision,
        "preset": "fast",
        "parameters": {"seed": 99, "ratio": 1.75}
    });
    let mut decided = client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Edit,
                arguments: Some(edited.clone()),
                reason: None,
                preview_token: Some(
                    client
                        .preview_assistant_confirmation(
                            &conversation,
                            &confirmation_id,
                            &genaimage_api::AssistantConfirmationPreviewRequest {
                                arguments: edited,
                            },
                        )
                        .await
                        .unwrap()
                        .preview_token,
                ),
            },
        )
        .await
        .unwrap();
    finish(&mut decided).await;
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    assert_eq!(graph["3"]["inputs"]["seed"], json!(99));
    assert_eq!(graph["3"]["inputs"]["ratio"].to_string(), "1.75");

    let rejection_revision = api
        .core
        .load_workflow_by_id("contract-workflow")
        .unwrap()
        .unwrap()
        .revision();
    let before = api.fake.prompt_calls.load(Ordering::SeqCst);
    let rejected_args = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": rejection_revision,
        "parameters": {"seed": 5, "steps": 10, "ratio": 1.5}
    });
    let (rejected, rejected_id, _rejected_token, _) =
        pause_submit(&llm, &client, rejected_args).await;
    let mut rejected_stream = client
        .decide_assistant_confirmation(
            &rejected,
            &rejected_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Reject,
                arguments: None,
                reason: Some("not approved".into()),
                preview_token: None,
            },
        )
        .await
        .unwrap();
    let mut rejection_seen = false;
    while let Some(event) = rejected_stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ToolResult(event) if !event.ok => rejection_seen = true,
            AssistantStreamEvent::Done(_) => break,
            AssistantStreamEvent::Error(event) => panic!("unexpected rejection error: {event:?}"),
            _ => {}
        }
    }
    assert!(rejection_seen);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn revision_mismatch_at_invocation_fails_without_submission() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    let (conversation, confirmation_id, preview_token, _) = pause_submit(
        &llm,
        &client,
        json!({
            "workflow_id": "contract-workflow",
            "workflow_revision": revision,
            "preset": "fast",
            "parameters": {"ratio": 2.0}
        }),
    )
    .await;
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["description"] = json!("changed after review");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    let error = match client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Confirm,
                arguments: None,
                reason: None,
                preview_token: Some(preview_token.clone()),
            },
        )
        .await
    {
        Ok(_) => panic!("stale confirmation unexpectedly succeeded"),
        Err(error) => error,
    };
    let Error::Problem(problem) = error else {
        panic!("expected stale-preview problem");
    };
    assert_eq!(problem.status, StatusCode::CONFLICT.as_u16());
    assert!(problem
        .preview
        .as_ref()
        .is_some_and(|preview| preview["valid"] == false));
    assert!(client
        .assistant_conversation(
            &conversation,
            &genaimage_api::AssistantConversationDetailQuery::default(),
        )
        .await
        .is_ok());
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api.core.list_runs(10).unwrap().is_empty());
    let audit = client
        .assistant_tool_calls(
            &conversation,
            &genaimage_api::AssistantToolAuditQuery::default(),
        )
        .await
        .unwrap();
    assert!(audit.tool_calls.iter().any(|row| {
        row.confirmation_id.as_deref() == Some(confirmation_id.as_str())
            && row.confirmation_status.as_deref() == Some("pending")
    }));
}

#[tokio::test]
async fn submit_without_confirmation_fails_closed_and_double_confirm_creates_one_run() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let submit = genaimage_core::assistant::tools::builtin_assistant_tools(api.core.clone(), roles)
        .into_iter()
        .find(|tool| tool.name() == "submit_run")
        .unwrap();
    let arguments = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": revision,
        "preset": "fast",
        "parameters": {"ratio": 2.0}
    });
    let error = submit
        .invoke(
            ToolContext {
                conversation_id: "conversation".into(),
                tool_call_id: "tool".into(),
                confirmation_id: None,
            },
            arguments.clone(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("requires a confirmation ID"));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);

    let client = client(&api);
    let (conversation, confirmation_id, preview_token, _) =
        pause_submit(&llm, &client, arguments).await;
    let decision = AssistantConfirmationRequest {
        decision: AssistantConfirmationDecisionKind::Confirm,
        arguments: None,
        reason: None,
        preview_token: Some(preview_token.clone()),
    };
    let mut first = client
        .decide_assistant_confirmation(&conversation, &confirmation_id, &decision)
        .await
        .unwrap();
    finish(&mut first).await;
    let error = match client
        .decide_assistant_confirmation(&conversation, &confirmation_id, &decision)
        .await
    {
        Ok(_) => panic!("double confirmation unexpectedly succeeded"),
        Err(error) => error,
    };
    match error {
        Error::Problem(problem) => assert_eq!(problem.status, StatusCode::CONFLICT.as_u16()),
        other => panic!("expected conflict, got {other}"),
    }
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.core.list_runs(10).unwrap().len(), 1);
}

#[tokio::test]
async fn simulated_crash_marks_outcome_unknown_and_never_reinvokes() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    let (conversation, confirmation_id, preview_token, _) = pause_submit(
        &llm,
        &client,
        json!({
            "workflow_id": "contract-workflow",
            "workflow_revision": revision,
            "preset": "fast",
            "parameters": {"ratio": 2.0}
        }),
    )
    .await;
    let connection = rusqlite::Connection::open(api.core.config().state_database.clone()).unwrap();
    connection.execute(
        "UPDATE assistant_tool_confirmations SET status = 'invoking', decision = 'confirm', effective_arguments_json = proposed_arguments_json, decided_at = created_at WHERE id = ?1",
        [&confirmation_id],
    ).unwrap();
    drop(connection);

    let detail = client
        .assistant_conversation(
            &conversation,
            &genaimage_api::AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    let last = detail.messages.last().unwrap();
    assert_eq!(last.role, genaimage_api::AssistantMessageRole::Tool);
    assert!(matches!(
        &last.content,
        genaimage_api::AssistantChatMessage::Tool { call_id, result }
            if call_id == "submit"
                && result == &json!({"ok": false, "error": "Outcome unknown. The process stopped after invocation started; check the external operation before continuing and never blindly retry it."})
    ));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    let error = client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Confirm,
                arguments: None,
                reason: None,
                preview_token: Some(preview_token.clone()),
            },
        )
        .await
        .err()
        .expect("outcome_unknown confirmation must not replay");
    assert!(
        matches!(error, Error::Problem(problem) if problem.status == StatusCode::CONFLICT.as_u16())
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn inspect_get_output_and_review_use_the_vision_role_only() {
    let (api, llm) = configured_with_builtin_tools().await;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    llm.set_scripts([
        tool_script(
            "inspect",
            "inspect_workflow",
            json!({"workflow_id": "contract-workflow"}),
        ),
        text_script("inspected"),
    ]);
    let conversation = create(&client).await;
    let mut inspection = client
        .send_assistant_message(&conversation.id, &message("inspect it"))
        .await
        .unwrap();
    let result = finish(&mut inspection).await;
    assert_eq!(result["revision"], revision);
    assert_eq!(result["parameters"]["source_image"]["user_supplied"], true);

    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    *api.fake.output_bytes.lock().unwrap() = Some(png.into_inner());
    api.fake.complete.store(false, Ordering::SeqCst);
    let run = api
        .core
        .submit(RunRequest {
            checkpoint_profile: None,
            checkpoint_profile_revision: None,
            workflow: "contract-workflow".into(),
            preset: Some("fast".into()),
            parameters: [("ratio".into(), json!(2.0))].into_iter().collect(),
            version: None,
            workflow_revision: Some(revision),
            character: None,
        })
        .await
        .unwrap();
    api.fake.complete.store(true, Ordering::SeqCst);
    api.core.reconcile(&run.id).await.unwrap();
    let outputs = api
        .core
        .repository()
        .as_async()
        .list_outputs(genaimage_core::media::OutputListQuery {
            run: Some(run.id.clone()),
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    let output_id = outputs.outputs[0].id.clone();

    llm.set_scripts([
        tool_script("output", "get_output", json!({"output_id": output_id})),
        text_script("metadata"),
    ]);
    let mut metadata = client
        .send_assistant_message(&conversation.id, &message("get metadata"))
        .await
        .unwrap();
    let result = finish(&mut metadata).await;
    assert_eq!(result["output_id"], output_id);
    assert!(result.get("local_path").is_none());
    assert!(result.get("bytes").is_none());

    let review = r#"{"caption":"A generated image","adherence_critique":"Matches the request.","tags":["generated"]}"#;
    llm.set_scripts([
        tool_script("review", "review_image", json!({"output_id": output_id})),
        text_script(review),
        text_script("reviewed"),
    ]);
    let mut stream = client
        .send_assistant_message(&conversation.id, &message("review the image"))
        .await
        .unwrap();
    let reviewed = finish(&mut stream).await;
    assert_eq!(reviewed["model"], VISION_MODEL);
    assert_eq!(reviewed["output_id"], output_id);
    let recorded = llm.recorded_requests();
    let image_requests = recorded
        .iter()
        .filter(|request| request.to_string().contains("image_url"))
        .collect::<Vec<_>>();
    assert_eq!(image_requests.len(), 1);
    assert_eq!(image_requests[0]["model"], VISION_MODEL);
    assert!(recorded
        .iter()
        .filter(|request| request["model"] == TEXT_MODEL)
        .all(|request| !request.to_string().contains("image_url")));
    assert!(api
        .core
        .get_run(&run.id)
        .unwrap()
        .status
        .eq(&RunStatus::Completed));
}

#[tokio::test]
async fn review_image_returns_an_error_when_vision_is_unavailable_without_text_fallback() {
    let llm = FakeLlmServer::start(text_script("ready")).await;
    llm.set_scripts([
        tool_script(
            "review-unavailable",
            "review_image",
            json!({"output_id": "run-missing:1"}),
        )
        .with_models_response(json!({
            "object": "list",
            "data": [
                {"id": TEXT_MODEL, "supports_tools": true, "supports_vision": false},
                {"id": VISION_MODEL, "supports_tools": false, "supports_vision": false}
            ]
        })),
        text_script("reported the unavailable vision role").with_models_response(json!({
            "object": "list",
            "data": [
                {"id": TEXT_MODEL, "supports_tools": true, "supports_vision": false},
                {"id": VISION_MODEL, "supports_tools": false, "supports_vision": false}
            ]
        })),
    ]);
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_builtin_assistant_tools(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", TEXT_MODEL),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
    )
    .await;
    let client = client(&api);
    let conversation = create(&client).await;
    let mut stream = client
        .send_assistant_message(&conversation.id, &message("review it"))
        .await
        .unwrap();
    let mut saw_error = false;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ToolResult(result) if !result.ok => {
                assert!(result.error.unwrap().contains("vision"));
                saw_error = true;
            }
            AssistantStreamEvent::Done(_) => break,
            AssistantStreamEvent::Error(event) => panic!("assistant failed: {event:?}"),
            _ => {}
        }
    }
    assert!(saw_error);
    let requests = llm.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request["model"] == TEXT_MODEL));
    assert!(requests
        .iter()
        .all(|request| !request.to_string().contains("image_url")));
}

#[tokio::test]
async fn edit_requires_a_fresh_token_bound_to_its_arguments() {
    let (_api, llm) = configured_with_builtin_tools().await;
    let api = _api;
    let revision = install_exact_workflow(&api);
    let client = client(&api);
    let original = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": revision,
        "preset": "fast",
        "parameters": {"seed": 10, "ratio": 1.0}
    });
    let (conversation, confirmation_id, original_token, _) =
        pause_submit(&llm, &client, original.clone()).await;
    assert!(!original_token.is_empty());

    let missing_confirm = client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Confirm,
                arguments: None,
                reason: None,
                preview_token: None,
            },
        )
        .await;
    assert!(matches!(
        missing_confirm,
        Err(Error::Problem(problem))
            if problem.status == StatusCode::UNPROCESSABLE_ENTITY.as_u16()
    ));

    let missing = client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Edit,
                arguments: Some(original.clone()),
                reason: None,
                preview_token: None,
            },
        )
        .await;
    let invalid_preview = client
        .preview_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &genaimage_api::AssistantConfirmationPreviewRequest {
                arguments: json!({
                    "workflow_id": "contract-workflow",
                    "workflow_revision": revision,
                    "preset": "fast",
                    "parameters": {"seed": 10, "steps": 0, "ratio": 1.0}
                }),
            },
        )
        .await;
    let invalid_preview = match invalid_preview {
        Ok(_) => panic!("invalid preview unexpectedly succeeded"),
        Err(Error::Problem(problem)) => problem,
        Err(error) => panic!("unexpected error: {error}"),
    };
    assert_eq!(
        invalid_preview.status,
        StatusCode::UNPROCESSABLE_ENTITY.as_u16()
    );
    assert!(invalid_preview.preview.is_some());

    let missing = match missing {
        Ok(_) => panic!("edit without token unexpectedly succeeded"),
        Err(Error::Problem(problem)) => problem,
        Err(error) => panic!("unexpected error: {error}"),
    };
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY.as_u16());

    let different = json!({
        "workflow_id": "contract-workflow",
        "workflow_revision": revision,
        "preset": "fast",
        "parameters": {"seed": 11, "ratio": 1.0}
    });
    let mismatched = client
        .decide_assistant_confirmation(
            &conversation,
            &confirmation_id,
            &AssistantConfirmationRequest {
                decision: AssistantConfirmationDecisionKind::Edit,
                arguments: Some(different.clone()),
                reason: None,
                preview_token: Some(original_token),
            },
        )
        .await;
    let mismatched = match mismatched {
        Ok(_) => panic!("token for different arguments unexpectedly succeeded"),
        Err(Error::Problem(problem)) => problem,
        Err(error) => panic!("unexpected error: {error}"),
    };
    assert_eq!(mismatched.status, StatusCode::UNPROCESSABLE_ENTITY.as_u16());
    assert!(mismatched.preview.is_some());
    assert!(mismatched.preview_token.is_some());
    assert!(
        client
            .preview_assistant_confirmation(
                &conversation,
                &confirmation_id,
                &genaimage_api::AssistantConfirmationPreviewRequest {
                    arguments: different
                },
            )
            .await
            .is_ok(),
        "confirmation must remain pending after rejected tokens"
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn propose_run_rejects_integer_above_an_exact_manifest_maximum() {
    let (api, _llm) = configured_with_builtin_tools().await;
    install_exact_workflow(&api);
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["parameters"]["seed"]["maximum"] = Value::Number("9007199254740992".parse().unwrap());
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let revision = api
        .core
        .load_workflow_by_id("contract-workflow")
        .unwrap()
        .unwrap()
        .revision();
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let propose =
        genaimage_core::assistant::tools::builtin_assistant_tools(api.core.clone(), roles)
            .into_iter()
            .find(|tool| tool.name() == "propose_run")
            .unwrap();
    let error = propose
        .invoke(
            ToolContext {
                conversation_id: "proposal".into(),
                tool_call_id: "propose".into(),
                confirmation_id: None,
            },
            json!({
                "workflow_id": "contract-workflow",
                "workflow_revision": revision,
                "parameters": {
                    "seed": 9007199254740993_u64,
                    "steps": 10,
                    "ratio": 1.5
                }
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("above maximum"), "{error}");
}

#[tokio::test]
async fn the_list_tool_omits_ids_claimed_by_several_manifests() {
    let (api, llm) = configured_with_builtin_tools().await;
    install_workflow(&api);
    let pack = api.core.config().packs[0].join("twin");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("pack.toml"),
        "id = 'twin'\nversion = '1'\nworkflows = ['a.genaimage.json', 'b.genaimage.json']\n",
    )
    .unwrap();
    std::fs::write(
        pack.join("workflow.api.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
    )
    .unwrap();
    for (file, id) in [("a", "contract-workflow"), ("b", "pack-only")] {
        let mut manifest: Value = serde_json::from_str(include_str!(
            "../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"
        ))
        .unwrap();
        manifest["id"] = json!(id);
        std::fs::write(
            pack.join(format!("{file}.genaimage.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
    let client = client(&api);
    let conversation = create(&client).await;
    llm.set_scripts([
        tool_script("list", "list_workflows", json!({})),
        text_script("listed"),
    ]);
    let mut stream = client
        .send_assistant_message(&conversation.id, &message("what can I run?"))
        .await
        .unwrap();
    let result = finish(&mut stream).await;
    let ids: Vec<_> = result["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workflow| workflow["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pack-only"], "a duplicated ID cannot be proposed");
}
