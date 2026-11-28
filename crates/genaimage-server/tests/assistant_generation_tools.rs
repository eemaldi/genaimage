mod common;

use std::{fs, path::PathBuf, sync::atomic::Ordering};

use common::ApiHarness;
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantEventStream, AssistantStreamEvent, Client, Error},
    AssistantChatContent, AssistantConfirmationDecisionKind, AssistantConfirmationRequest,
    AssistantToolConfirmationStatus, CreateAssistantConversationRequest,
    SendAssistantMessageRequest,
};
use genaimage_core::{
    assistant::tools::builtin_assistant_tools, AssistantTool, LlmRoles, RunRecord, RunRequest,
    RunStatus, ToolContext,
};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

const TEXT_MODEL: &str = "assistant-text";
const VISION_MODEL: &str = "assistant-vision";
const CHECKPOINT: &str = "b.safetensors";
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
"#;

fn models_script(events: Vec<FakeLlmEvent>) -> FakeLlmScript {
    FakeLlmScript::default()
        .with_events(events)
        .with_models_response(json!({
            "object": "list",
            "data": [
                {"id": TEXT_MODEL, "supports_tools": true, "supports_vision": false},
                {"id": VISION_MODEL, "supports_tools": false, "supports_vision": true}
            ]
        }))
}

fn tool_script(id: &str, name: &str, arguments: Value) -> FakeLlmScript {
    models_script(vec![
        FakeLlmEvent::tool_calls([FakeToolCallDelta::new(0)
            .id(id)
            .name(name)
            .arguments(serde_json::to_string(&arguments).unwrap())]),
        FakeLlmEvent::finish("tool_calls"),
        FakeLlmEvent::Done,
    ])
}

fn text_script(text: &str) -> FakeLlmScript {
    models_script(vec![
        FakeLlmEvent::text(text),
        FakeLlmEvent::finish("stop"),
        FakeLlmEvent::Done,
    ])
}

async fn start_with(extra_env: &[(&str, &str)]) -> (ApiHarness, FakeLlmServer) {
    let llm = FakeLlmServer::start(text_script("ready")).await;
    let llm_url = llm.base_url();
    let mut env = vec![
        ("GENAIMAGE_LLM_URL", llm_url.as_str()),
        ("GENAIMAGE_LLM_TEXT_MODEL", TEXT_MODEL),
        ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
    ];
    env.extend_from_slice(extra_env);
    let api = ApiHarness::start_with_builtin_assistant_tools(5, &env).await;
    (api, llm)
}

async fn start() -> (ApiHarness, FakeLlmServer) {
    let started = start_with(&[]).await;
    install_pack(&started.0);
    started
}

fn client(api: &ApiHarness) -> Client {
    Client::with_base_url(&api.origin).unwrap()
}

fn message(text: &str) -> SendAssistantMessageRequest {
    SendAssistantMessageRequest {
        content: vec![AssistantChatContent::Text(text.to_owned())],
    }
}

fn pack_dir(api: &ApiHarness) -> PathBuf {
    api.core.config().packs[0].join("gen")
}

fn install_pack(api: &ApiHarness) {
    let dir = pack_dir(api);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("pack.toml"),
        "id = 'gen'\nversion = '1'\nworkflows = ['wf.genaimage.json']\ncheckpoint_profiles = 'checkpoints.toml'\n",
    )
    .unwrap();
    fs::write(
        dir.join("workflow.api.json"),
        r#"{"3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7.5, "shift": -0.0}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "a.ckpt"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "g"}}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("wf.genaimage.json"),
        r#"{
            "schema_version": 2, "id": "gen-flow", "version": "1", "workflow": "workflow.api.json",
            "parameters": {
                "seed": {"node": "3", "input": "seed", "kind": "seed", "default": 9007199254740993},
                "steps": {"node": "3", "input": "steps", "kind": "integer", "default": 20, "minimum": 1, "maximum": 100},
                "cfg": {"node": "3", "input": "cfg", "kind": "number", "default": 7.5},
                "shift": {"node": "3", "input": "shift", "kind": "number", "required": false},
                "checkpoint": {"node": "4", "input": "ckpt_name", "kind": "string", "default": "b.safetensors"}
            },
            "presets": {"portrait": {"steps": 30}},
            "preset_info": {"portrait": {"label": "Portrait 2:3", "description": "Tall canvas", "kind": "layout"}},
            "outputs": [{"node": "9", "kind": "image"}]
        }"#,
    )
    .unwrap();
    fs::write(dir.join("checkpoints.toml"), PROFILES).unwrap();
    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "KSampler": {},
        "SaveImage": {},
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [[CHECKPOINT], {}]}}}
    }));
}

fn workflow_revision(api: &ApiHarness, id: &str) -> String {
    api.core
        .load_workflow_by_id(id)
        .unwrap()
        .unwrap()
        .revision()
}

fn profile_revision(api: &ApiHarness, id: &str) -> String {
    let workflow = api.core.load_workflow_by_id("gen-flow").unwrap().unwrap();
    api.core
        .pack_of(&workflow)
        .unwrap()
        .checkpoint_profiles
        .iter()
        .find(|profile| profile.id == id)
        .unwrap()
        .revision()
}

async fn create_character(api: &ApiHarness) -> String {
    let response = api
        .request(Method::POST, "/api/v1/characters")
        .json(&json!({"name": "Mara", "spec": {
            "summary": "A lighthouse keeper", "elements": [], "palette": [],
            "views": ["front"], "style_notes": "Gouache"
        }}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let created: Value = response.json().await.unwrap();
    created["character"]["id"].as_str().unwrap().to_owned()
}

fn run_arguments(api: &ApiHarness) -> Value {
    json!({
        "workflow_id": "gen-flow",
        "workflow_revision": workflow_revision(api, "gen-flow"),
        "preset": "portrait",
        "parameters": {"seed": 9007199254740993_u64, "checkpoint": CHECKPOINT},
        "checkpoint_profile": "base",
        "checkpoint_profile_revision": profile_revision(api, "base")
    })
}

fn tool(api: &ApiHarness, name: &str) -> std::sync::Arc<dyn AssistantTool> {
    let roles = LlmRoles::new(&api.core.config().llm, None);
    builtin_assistant_tools(api.core.clone(), roles)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

async fn preview(api: &ApiHarness, name: &str, arguments: &Value) -> genaimage_core::Result<Value> {
    Ok(tool(api, name)
        .preview(arguments)
        .await?
        .expect("a confirmation tool previews"))
}

async fn invoke(
    api: &ApiHarness,
    name: &str,
    arguments: &Value,
    confirmation_id: Option<&str>,
) -> genaimage_core::Result<Value> {
    tool(api, name)
        .invoke(
            ToolContext {
                conversation_id: "conversation".into(),
                tool_call_id: "call".into(),
                confirmation_id: confirmation_id.map(str::to_owned),
            },
            arguments.clone(),
        )
        .await
}

fn value(preview: &Value, parameter: &str) -> Value {
    preview["effective_values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["parameter"] == parameter)
        .unwrap_or_else(|| panic!("no effective value for {parameter}: {preview}"))
        .clone()
}

fn nothing_was_submitted(api: &ApiHarness) {
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api.core.list_runs(10).unwrap().is_empty());
}

#[tokio::test]
async fn the_preview_shows_every_value_with_its_source_and_every_pin() {
    let (api, _llm) = start().await;
    let character = create_character(&api).await;
    let mut arguments = run_arguments(&api);
    arguments["character"] = json!({"id": character, "spec_version": 1});

    let preview = preview(&api, "submit_run", &arguments).await.unwrap();
    let again = self::preview(&api, "submit_run", &arguments).await.unwrap();
    assert_eq!(preview, again, "a preview is deterministic");
    assert_eq!(
        preview["checkpoint_profile"],
        json!({
            "id": "base",
            "revision": profile_revision(&api, "base"),
            "ignored": ["unrelated"]
        })
    );
    assert_eq!(
        preview["character"],
        json!({"id": character, "name": "Mara", "spec_version": 1, "latest_spec_version": 1})
    );
    assert_eq!(preview["workflow_revision"], arguments["workflow_revision"]);

    assert_eq!(
        value(&preview, "seed"),
        json!({"parameter": "seed", "value": 9007199254740993_u64, "source": "user"})
    );
    assert_eq!(value(&preview, "checkpoint")["source"], "user");
    let steps = value(&preview, "steps");
    assert_eq!(
        (steps["value"].clone(), steps["source"].clone()),
        (json!(30), json!("preset"))
    );
    let cfg = value(&preview, "cfg");
    assert_eq!(cfg["source"], "checkpoint_profile");
    assert_eq!(cfg["checkpoint_profile"], "base");
    assert_eq!(cfg["value"].to_string(), "2.0");
    assert!(preview["effective_values"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["parameter"] != "shift"));
    let inherited = preview["inherited_inputs"].as_array().unwrap();
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0]["parameter"], "shift");
    assert_eq!(inherited[0]["source"], "workflow");
    assert_eq!(inherited[0]["value"].to_string(), "-0.0");
    assert_eq!(preview["parameters"]["shift"].to_string(), "-0.0");
    assert_eq!(preview["parameters"]["cfg"].to_string(), "2.0");
    assert_eq!(preview["parameters"]["steps"], 30);

    let proposed = invoke(&api, "propose_run", &arguments, None).await.unwrap();
    assert_eq!(proposed["effective_values"], preview["effective_values"]);
    assert_eq!(
        proposed["checkpoint_profile"],
        preview["checkpoint_profile"]
    );
    nothing_was_submitted(&api);

    let mut plain = arguments.clone();
    for key in [
        "checkpoint_profile",
        "checkpoint_profile_revision",
        "character",
    ] {
        plain.as_object_mut().unwrap().remove(key);
    }
    let plain = self::preview(&api, "submit_run", &plain).await.unwrap();
    assert_eq!(plain["checkpoint_profile"], Value::Null);
    assert_eq!(plain["character"], Value::Null);
    assert_eq!(
        value(&plain, "cfg")["source"],
        "default",
        "no profile unless named"
    );
    assert_eq!(value(&plain, "cfg")["value"].to_string(), "7.5");
}

#[tokio::test]
async fn a_profile_needs_its_revision_and_a_revision_needs_its_profile() {
    let (api, _llm) = start().await;
    let mut profile_only = run_arguments(&api);
    profile_only
        .as_object_mut()
        .unwrap()
        .remove("checkpoint_profile_revision");
    let mut revision_only = run_arguments(&api);
    revision_only
        .as_object_mut()
        .unwrap()
        .remove("checkpoint_profile");
    for (arguments, expected) in [
        (&profile_only, "together with checkpoint_profile_revision"),
        (&revision_only, "without checkpoint_profile"),
    ] {
        for name in ["propose_run", "submit_run"] {
            let error = match name {
                "propose_run" => invoke(&api, name, arguments, None).await,
                _ => preview(&api, name, arguments).await,
            }
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
        let error = invoke(&api, "submit_run", arguments, Some("confirmation"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
    nothing_was_submitted(&api);
}

#[tokio::test]
async fn a_profile_that_does_not_apply_is_refused_and_never_guessed() {
    let (api, _llm) = start().await;
    let mut other_checkpoint = run_arguments(&api);
    other_checkpoint["parameters"]["checkpoint"] = json!("a.ckpt");
    let error = preview(&api, "submit_run", &other_checkpoint)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("does not list checkpoint 'a.ckpt'"),
        "{error}"
    );

    let mut unknown = run_arguments(&api);
    unknown["checkpoint_profile"] = json!("missing");
    let error = preview(&api, "submit_run", &unknown)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("has no checkpoint profile 'missing'"),
        "{error}"
    );

    let mut stale = run_arguments(&api);
    stale["checkpoint_profile_revision"] = json!("sha256:stale");
    let error = preview(&api, "submit_run", &stale)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("checkpoint profile changed"), "{error}");
    let error = invoke(&api, "submit_run", &stale, Some("key"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("checkpoint profile changed"), "{error}");

    let mut stale_workflow = run_arguments(&api);
    stale_workflow["workflow_revision"] = json!("sha256:stale");
    assert!(preview(&api, "submit_run", &stale_workflow).await.is_err());

    let mut image = run_arguments(&api);
    image["parameters"]["unknown_parameter"] = json!(1);
    assert!(preview(&api, "submit_run", &image).await.is_err());
    nothing_was_submitted(&api);
}

#[tokio::test]
async fn an_unknown_character_or_spec_version_is_refused_before_anything_is_written() {
    let (api, _llm) = start().await;
    let character = create_character(&api).await;
    for link in [
        json!({"id": "missing", "spec_version": 1}),
        json!({"id": character, "spec_version": 2}),
    ] {
        let mut arguments = run_arguments(&api);
        arguments["character"] = link.clone();
        let error = preview(&api, "submit_run", &arguments)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("was not found") || error.contains("has no spec version"),
            "{link}: {error}"
        );
        let error = invoke(&api, "submit_run", &arguments, Some("key"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("was not found") || error.contains("has no spec version"),
            "{link}: {error}"
        );
    }
    nothing_was_submitted(&api);
}

#[tokio::test]
async fn a_malformed_character_link_is_returned_to_the_model_without_a_confirmation() {
    let (api, llm) = start().await;
    let client = client(&api);
    let character = create_character(&api).await;
    for link in [
        json!({"id": character}),
        json!({"id": character, "spec_version": 0}),
    ] {
        let mut arguments = run_arguments(&api);
        arguments["character"] = link;
        let conversation = create_conversation(&client).await;
        llm.set_scripts([
            tool_script("call", "submit_run", arguments),
            text_script("fixing it"),
        ]);
        let mut stream = client
            .send_assistant_message(&conversation, &message("submit"))
            .await
            .unwrap();
        let (ok, result) = finish(&mut stream).await;
        assert!(!ok, "{result}");
    }
    nothing_was_submitted(&api);
}

#[tokio::test]
async fn retrying_with_the_same_confirmation_id_gives_one_run() {
    let (api, _llm) = start().await;
    let character = create_character(&api).await;
    let mut arguments = run_arguments(&api);
    arguments["character"] = json!({"id": character, "spec_version": 1});
    let first = invoke(&api, "submit_run", &arguments, Some("confirmation-1"))
        .await
        .unwrap();
    let second = invoke(&api, "submit_run", &arguments, Some("confirmation-1"))
        .await
        .unwrap();
    assert_eq!(first["run_id"], second["run_id"]);
    assert_eq!(
        (first["replayed"].clone(), second["replayed"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(
        first["checkpoint_profile"]["revision"],
        arguments["checkpoint_profile_revision"]
    );
    assert_eq!(
        first["character"],
        json!({"id": character, "spec_version": 1})
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.core.list_runs(10).unwrap().len(), 1);

    let mut other = arguments.clone();
    other["parameters"]["seed"] = json!(7);
    let error = invoke(&api, "submit_run", &other, Some("confirmation-1"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("different request"), "{error}");
    assert_eq!(api.core.list_runs(10).unwrap().len(), 1);

    let error = invoke(&api, "submit_run", &arguments, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a confirmation ID"), "{error}");
}

#[tokio::test]
async fn a_recorded_failure_is_an_error_that_names_the_run_not_a_success() {
    let (api, _llm) = start().await;
    let arguments = run_arguments(&api);
    api.fake.fail_submit_response.store(true, Ordering::SeqCst);
    let error = invoke(&api, "submit_run", &arguments, Some("failing"))
        .await
        .unwrap_err()
        .to_string();
    let run = api.core.list_runs(10).unwrap().remove(0);
    assert!(error.contains(&run.id), "{error}");
    assert!(error.contains(run.status.as_str()), "{error}");
    assert_ne!(run.status, RunStatus::Queued);
}

async fn create_conversation(client: &Client) -> String {
    client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Generation tools".into(),
        })
        .await
        .unwrap()
        .id
}

async fn pause(
    stream: &mut AssistantEventStream,
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

/// The last tool result of the stream, successful or not.
async fn finish(stream: &mut AssistantEventStream) -> (bool, Value) {
    let mut result = (false, Value::Null);
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ToolResult(event) => {
                result = (
                    event.ok,
                    event
                        .result
                        .unwrap_or_else(|| json!({"error": event.error})),
                );
            }
            AssistantStreamEvent::Done(_) => return result,
            AssistantStreamEvent::Error(event) => panic!("assistant failed: {event:?}"),
            _ => {}
        }
    }
    panic!("assistant stream ended without done")
}

struct Paused {
    conversation: String,
    confirmation_id: String,
    preview_token: String,
    preview: Value,
}

async fn pause_tool(llm: &FakeLlmServer, client: &Client, tool: &str, arguments: Value) -> Paused {
    let conversation = create_conversation(client).await;
    llm.set_scripts([tool_script("call", tool, arguments), text_script("done")]);
    let mut stream = client
        .send_assistant_message(&conversation, &message("go ahead"))
        .await
        .unwrap();
    let required = pause(&mut stream).await;
    assert_eq!(required.status, AssistantToolConfirmationStatus::Pending);
    Paused {
        conversation,
        confirmation_id: required.confirmation_id,
        preview_token: required.preview_token,
        preview: required.preview.expect("a preview"),
    }
}

fn confirm(paused: &Paused) -> AssistantConfirmationRequest {
    AssistantConfirmationRequest {
        decision: AssistantConfirmationDecisionKind::Confirm,
        arguments: None,
        reason: None,
        preview_token: Some(paused.preview_token.clone()),
    }
}

async fn decide(client: &Client, paused: &Paused) -> (bool, Value) {
    let mut stream = client
        .decide_assistant_confirmation(
            &paused.conversation,
            &paused.confirmation_id,
            &confirm(paused),
        )
        .await
        .unwrap();
    finish(&mut stream).await
}

fn problem(error: Error) -> genaimage_api::ProblemDetails {
    match error {
        Error::Problem(problem) => *problem,
        other => panic!("expected a problem, got {other}"),
    }
}

fn preflight(run: &RunRecord) -> Value {
    serde_json::from_slice(
        &fs::read(PathBuf::from(&run.output_dir).join("preflight.json")).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_confirmed_submission_pins_the_profile_and_links_the_character_in_provenance() {
    let (api, llm) = start().await;
    let client = client(&api);
    let character = create_character(&api).await;
    let mut arguments = run_arguments(&api);
    arguments["character"] = json!({"id": character, "spec_version": 1});
    let paused = pause_tool(&llm, &client, "submit_run", arguments.clone()).await;
    assert_eq!(paused.preview["checkpoint_profile"]["id"], "base");
    assert_eq!(
        value(&paused.preview, "cfg")["source"],
        "checkpoint_profile"
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);

    let (ok, result) = decide(&client, &paused).await;
    assert!(ok, "{result}");
    let run_id = result["run_id"].as_str().unwrap();
    assert_eq!(result["status"], "queued");
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    let run = api.core.get_run(run_id).unwrap();
    assert_eq!(run.character.as_ref().unwrap().id, character);
    assert_eq!(run.character.as_ref().unwrap().spec_version, 1);
    let recorded = preflight(&run);
    assert_eq!(recorded["checkpoint_profile"], "base");
    assert_eq!(
        recorded["checkpoint_profile_revision"],
        arguments["checkpoint_profile_revision"]
    );
    assert_eq!(
        recorded["character"],
        json!({"id": character, "spec_version": 1})
    );
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    assert_eq!(graph["3"]["inputs"]["seed"], json!(9007199254740993_u64));
    assert_eq!(
        graph["3"]["inputs"]["steps"], 30,
        "the preset beats the profile"
    );
    assert_eq!(
        graph["3"]["inputs"]["cfg"].to_string(),
        "2.0",
        "the profile beats the default"
    );
    assert_eq!(graph["3"]["inputs"]["shift"].to_string(), "-0.0");
    let connection = rusqlite::Connection::open(api.core.config().state_database.clone()).unwrap();
    let key: String = connection
        .query_row("SELECT key FROM run_submission_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        key, paused.confirmation_id,
        "the confirmation is the idempotency key"
    );
}

#[tokio::test]
async fn a_profile_revision_that_changed_after_review_is_refused_at_decision_time() {
    let (api, llm) = start().await;
    let client = client(&api);
    let paused = pause_tool(&llm, &client, "submit_run", run_arguments(&api)).await;
    let path = pack_dir(&api).join("checkpoints.toml");
    fs::write(&path, PROFILES.replace("steps = 28", "steps = 29")).unwrap();

    let error = client
        .decide_assistant_confirmation(
            &paused.conversation,
            &paused.confirmation_id,
            &confirm(&paused),
        )
        .await
        .err()
        .expect("a changed profile must be refused");
    let problem = problem(error);
    assert_eq!(problem.status, StatusCode::CONFLICT.as_u16(), "{problem:?}");
    assert!(problem
        .preview
        .as_ref()
        .is_some_and(|preview| preview["valid"] == false));
    nothing_was_submitted(&api);
    let audit = client
        .assistant_tool_calls(
            &paused.conversation,
            &genaimage_api::AssistantToolAuditQuery::default(),
        )
        .await
        .unwrap();
    assert!(audit.tool_calls.iter().any(|row| {
        row.confirmation_id.as_deref() == Some(paused.confirmation_id.as_str())
            && row.confirmation_status.as_deref() == Some("pending")
    }));
}

#[tokio::test]
async fn an_interrupted_invocation_settles_as_outcome_unknown_and_is_never_replayed() {
    let (api, llm) = start().await;
    let client = client(&api);
    let queued = api.core.submit(plain_request(&api)).await.unwrap();
    for (tool_name, arguments) in [
        ("submit_run", run_arguments(&api)),
        ("cancel_run", json!({"run_id": queued.id})),
    ] {
        let paused = pause_tool(&llm, &client, tool_name, arguments).await;
        let connection =
            rusqlite::Connection::open(api.core.config().state_database.clone()).unwrap();
        connection.execute(
            "UPDATE assistant_tool_confirmations SET status = 'invoking', decision = 'confirm', effective_arguments_json = proposed_arguments_json, decided_at = created_at WHERE id = ?1",
            [&paused.confirmation_id],
        ).unwrap();
        drop(connection);
        let detail = client
            .assistant_conversation(
                &paused.conversation,
                &genaimage_api::AssistantConversationDetailQuery::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(
                &detail.messages.last().unwrap().content,
                genaimage_api::AssistantChatMessage::Tool { result, .. }
                    if result["ok"] == false && result["error"].as_str().unwrap().starts_with("Outcome unknown.")
            ),
            "{tool_name}"
        );
        let error = client
            .decide_assistant_confirmation(
                &paused.conversation,
                &paused.confirmation_id,
                &confirm(&paused),
            )
            .await
            .err()
            .expect("an outcome_unknown confirmation must not replay");
        assert_eq!(problem(error).status, StatusCode::CONFLICT.as_u16());
    }
    assert_eq!(
        api.fake.prompt_calls.load(Ordering::SeqCst),
        1,
        "only the setup run"
    );
    assert_eq!(api.fake.interrupt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.fake.delete_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        api.core.get_run(&queued.id).unwrap().status,
        RunStatus::Queued
    );
}

fn plain_request(api: &ApiHarness) -> RunRequest {
    RunRequest {
        workflow: "gen-flow".into(),
        preset: None,
        parameters: [("checkpoint".to_owned(), json!(CHECKPOINT))].into(),
        version: None,
        workflow_revision: Some(workflow_revision(api, "gen-flow")),
        checkpoint_profile: None,
        checkpoint_profile_revision: None,
        character: None,
    }
}

#[tokio::test]
async fn cancel_run_asks_for_confirmation_and_never_claims_a_cancellation_that_is_unconfirmed() {
    for running in [false, true] {
        let (api, llm) = start().await;
        let client = client(&api);
        api.fake.running.store(running, Ordering::SeqCst);
        let run = api.core.submit(plain_request(&api)).await.unwrap();
        let paused = pause_tool(&llm, &client, "cancel_run", json!({"run_id": run.id})).await;
        assert_eq!(paused.preview["run_id"], run.id);
        assert_eq!(paused.preview["workflow_id"], "gen-flow");
        assert_eq!(paused.preview["status"], "queued");
        assert_eq!(api.fake.interrupt_calls.load(Ordering::SeqCst), 0);
        assert_eq!(api.fake.delete_calls.load(Ordering::SeqCst), 0);

        let (ok, result) = decide(&client, &paused).await;
        assert!(ok, "{result}");
        assert_eq!(result["status"], "cancelling");
        assert_eq!(result["cancellation_requested"], true);
        assert_eq!(result["cancelled"], false);
        assert!(result["note"]
            .as_str()
            .unwrap()
            .contains("do not say it is cancelled"));
        assert_eq!(
            api.fake.interrupt_calls.load(Ordering::SeqCst),
            usize::from(running)
        );
        assert_eq!(
            api.fake.delete_calls.load(Ordering::SeqCst),
            usize::from(!running)
        );
        assert_eq!(
            api.core.get_run(&run.id).unwrap().status,
            RunStatus::Cancelling
        );
    }
}

#[tokio::test]
async fn cancel_run_refuses_finished_and_unknown_runs_and_a_status_that_changed() {
    let (api, llm) = start().await;
    let client = client(&api);
    let run = api.core.submit(plain_request(&api)).await.unwrap();
    let paused = pause_tool(&llm, &client, "cancel_run", json!({"run_id": run.id})).await;

    api.fake.complete.store(true, Ordering::SeqCst);
    let reconciled = api.core.reconcile(&run.id).await.unwrap().run;
    assert_eq!(reconciled.status, RunStatus::Completed);
    let error = problem(
        client
            .decide_assistant_confirmation(
                &paused.conversation,
                &paused.confirmation_id,
                &confirm(&paused),
            )
            .await
            .err()
            .expect("a stale cancellation must be refused"),
    );
    assert_eq!(
        error.status,
        StatusCode::UNPROCESSABLE_ENTITY.as_u16(),
        "{error:?}"
    );
    assert!(
        error
            .detail
            .contains("already completed and cannot be cancelled"),
        "{}",
        error.detail
    );

    let arguments = json!({"run_id": run.id});
    let error = preview(&api, "cancel_run", &arguments)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("already completed and cannot be cancelled"),
        "{error}"
    );
    let error = invoke(&api, "cancel_run", &arguments, Some("race"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("already completed and cannot be cancelled"),
        "{error}"
    );
    assert_eq!(api.fake.interrupt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.fake.delete_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        api.core.get_run(&run.id).unwrap().status,
        RunStatus::Completed
    );

    let missing = json!({"run_id": "run-missing"});
    let error = preview(&api, "cancel_run", &missing)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("was not found"), "{error}");
    let error = invoke(&api, "cancel_run", &missing, Some("key"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("was not found"), "{error}");
    let error = invoke(&api, "cancel_run", &arguments, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a confirmation ID"), "{error}");

    api.fake.complete.store(false, Ordering::SeqCst);
    let second = api.core.submit(plain_request(&api)).await.unwrap();
    api.core.cancel(&second.id).await.unwrap();
    let error = preview(&api, "cancel_run", &json!({"run_id": second.id}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("already requested"), "{error}");
}

#[tokio::test]
async fn the_cancel_and_run_tools_require_confirmation_and_run_status_follows_a_cancellation() {
    let (api, llm) = start().await;
    let client = client(&api);
    for name in ["submit_run", "cancel_run"] {
        assert!(tool(&api, name).requires_confirmation(), "{name}");
    }
    assert!(!tool(&api, "propose_run").requires_confirmation());
    api.fake.running.store(true, Ordering::SeqCst);
    let run = api.core.submit(plain_request(&api)).await.unwrap();
    let paused = pause_tool(&llm, &client, "cancel_run", json!({"run_id": run.id})).await;
    let (ok, _) = decide(&client, &paused).await;
    assert!(ok);
    api.fake.cancellation_history.store(true, Ordering::SeqCst);
    llm.set_scripts([
        tool_script("status", "run_status", json!({"run_id": run.id})),
        text_script("cancelled"),
    ]);
    let mut stream = client
        .send_assistant_message(&paused.conversation, &message("is it cancelled?"))
        .await
        .unwrap();
    let (ok, status) = finish(&mut stream).await;
    assert!(ok);
    assert_eq!(status["status"], "cancelled");
}

const SDXL: &str = "sd_xl_base_1.0.safetensors";

fn node_inputs(graph: &Value, class: &str) -> Value {
    graph
        .as_object()
        .unwrap()
        .values()
        .find(|node| node["class_type"] == class)
        .unwrap_or_else(|| panic!("no {class} node"))["inputs"]
        .clone()
}

#[tokio::test]
async fn chat_only_character_portrait_reads_the_spec_then_submits_with_layout_profile_and_character(
) {
    let packs = fs::canonicalize(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../workflows/packs"
    ))
    .unwrap()
    .display()
    .to_string();
    let (api, llm) = start_with(&[("GENAIMAGE_PACKS", packs.as_str())]).await;
    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "CheckpointLoaderSimple": {}, "CLIPTextEncode": {}, "EmptyLatentImage": {},
        "KSampler": {}, "SaveImage": {}, "VAEDecode": {}
    }));
    let client = client(&api);
    let created: Value = api
        .request(Method::POST, "/api/v1/characters")
        .json(&json!({"name": "Mara", "spec": {
            "summary": "A lighthouse keeper", "style_notes": "Gouache", "palette": [],
            "views": ["front"],
            "elements": [{
                "id": "coat", "category": "clothing",
                "description": "A salt-stained yellow oilskin coat", "requirement": "mandatory"
            }]
        }}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let character = created["character"]["id"].as_str().unwrap().to_owned();
    let workflow = api
        .core
        .load_workflow_by_id("character-portrait")
        .unwrap()
        .unwrap();
    let profile = api
        .core
        .pack_of(&workflow)
        .unwrap()
        .checkpoint_profiles
        .into_iter()
        .find(|profile| profile.id == "sdxl-base")
        .unwrap();
    let arguments = json!({
        "workflow_id": "character-portrait",
        "workflow_revision": workflow.revision(),
        "preset": "portrait-2x3",
        "parameters": {"checkpoint": SDXL, "prompt": "Mara in her yellow oilskin coat"},
        "checkpoint_profile": "sdxl-base",
        "checkpoint_profile_revision": profile.revision(),
        "character": {"id": character, "spec_version": 1}
    });
    llm.set_scripts([
        tool_script(
            "spec",
            "get_character_spec",
            json!({"character_id": character}),
        ),
        tool_script(
            "inspect",
            "inspect_workflow",
            json!({"workflow_id": "character-portrait"}),
        ),
        tool_script("propose", "propose_run", arguments.clone()),
        tool_script("submit", "submit_run", arguments.clone()),
        text_script("Submitted."),
    ]);
    let conversation = create_conversation(&client).await;
    let mut stream = client
        .send_assistant_message(&conversation, &message("Draw Mara in a 2:3 portrait"))
        .await
        .unwrap();
    let mut results = Vec::new();
    let mut required = None;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AssistantStreamEvent::ToolResult(event) => {
                assert!(event.ok, "{} failed: {:?}", event.tool, event.error);
                results.push((event.tool, event.result.unwrap()));
            }
            AssistantStreamEvent::ConfirmationRequired(event) => required = Some(event),
            AssistantStreamEvent::AwaitingConfirmation(_) => break,
            AssistantStreamEvent::Error(event) => panic!("assistant failed: {event:?}"),
            _ => {}
        }
    }
    let tools: Vec<_> = results.iter().map(|(tool, _)| tool.as_str()).collect();
    assert_eq!(
        tools,
        ["get_character_spec", "inspect_workflow", "propose_run"]
    );
    assert_eq!(results[0].1["complete"], true);
    assert_eq!(results[0].1["version"], 1);
    assert_eq!(
        results[2].1["checkpoint_profile"]["revision"],
        profile.revision()
    );
    assert_eq!(
        api.fake.prompt_calls.load(Ordering::SeqCst),
        0,
        "nothing runs before confirmation"
    );

    let required = required.expect("submit_run asks for confirmation");
    let preview = required.preview.clone().unwrap();
    for (parameter, expected, source) in [
        ("width", json!(832), "preset"),
        ("height", json!(1248), "preset"),
        ("steps", json!(30), "checkpoint_profile"),
        ("sampler_name", json!("dpmpp_2m"), "checkpoint_profile"),
        ("checkpoint", json!(SDXL), "user"),
        ("seed", json!(42), "default"),
    ] {
        let row = value(&preview, parameter);
        assert_eq!(
            (row["value"].clone(), row["source"].clone()),
            (expected, json!(source)),
            "{parameter}"
        );
    }
    assert_eq!(preview["character"]["spec_version"], 1);
    let paused = Paused {
        conversation,
        confirmation_id: required.confirmation_id,
        preview_token: required.preview_token,
        preview,
    };
    let (ok, submitted) = decide(&client, &paused).await;
    assert!(ok, "{submitted}");
    let run = api
        .core
        .get_run(submitted["run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(run.character.as_ref().unwrap().spec_version, 1);
    assert_eq!(
        preflight(&run)["checkpoint_profile_revision"],
        profile.revision()
    );
    let graph = api.fake.submitted_graph.lock().unwrap().clone().unwrap();
    let latent = node_inputs(&graph, "EmptyLatentImage");
    assert_eq!(
        (latent["width"].clone(), latent["height"].clone()),
        (json!(832), json!(1248))
    );
    let sampler = node_inputs(&graph, "KSampler");
    assert_eq!(sampler["steps"], 30);
    assert_eq!(sampler["cfg"].to_string(), "7.0");
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
}
