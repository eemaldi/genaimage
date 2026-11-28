mod common;

use std::{fs, io::Cursor, sync::atomic::Ordering};

use common::{install_workflow, ApiHarness};
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantStreamEvent, Client},
    AssistantChatContent, CreateAssistantConversationRequest, SendAssistantMessageRequest,
};
use genaimage_core::{assistant::tools::builtin_assistant_tools, LlmRoles, ToolContext};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::{multipart, Method};
use serde_json::{json, Value};

const TEXT_MODEL: &str = "assistant-text";
const VISION_MODEL: &str = "assistant-vision";

fn spec() -> Value {
    json!({
        "summary": "A lighthouse keeper", "elements": [], "palette": [],
        "views": ["front"], "style_notes": "Gouache"
    })
}
const PNG: &[u8] = include_bytes!("fixtures/reference.png");

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

async fn start() -> (ApiHarness, FakeLlmServer) {
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

async fn invoke(api: &ApiHarness, name: &str, arguments: Value) -> genaimage_core::Result<Value> {
    let roles = LlmRoles::new(&api.core.config().llm, None);
    let tool = builtin_assistant_tools(api.core.clone(), roles)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool {name}"));
    assert!(!tool.requires_confirmation(), "{name} is read-only");
    jsonschema_accepts(&tool.json_schema());
    tool.invoke(
        ToolContext {
            conversation_id: "c".into(),
            tool_call_id: "t".into(),
            confirmation_id: None,
        },
        arguments,
    )
    .await
}

fn jsonschema_accepts(schema: &Value) {
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
}

fn install_pack(api: &ApiHarness) {
    let dir = api.core.config().packs[0].join("chars");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("pack.toml"),
        "id = 'chars'\nversion = '1'\nworkflows = ['wf.genaimage.json']\ncheckpoint_profiles = 'checkpoints.toml'\n",
    )
    .unwrap();
    fs::write(
        dir.join("workflow.api.json"),
        r#"{"3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7.5, "shift": 0.0}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "a.ckpt"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "g"}}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("wf.genaimage.json"),
        r#"{
            "schema_version": 2, "id": "chars-flow", "version": "1", "workflow": "workflow.api.json",
            "parameters": {
                "seed": {"node": "3", "input": "seed", "kind": "seed", "default": 9007199254740993},
                "steps": {"node": "3", "input": "steps", "kind": "integer", "default": 20, "minimum": 1, "maximum": 100},
                "cfg": {"node": "3", "input": "cfg", "kind": "number", "default": 7.5},
                "shift": {"node": "3", "input": "shift", "kind": "number", "default": -0.0},
                "checkpoint": {"node": "4", "input": "ckpt_name", "kind": "string", "default": "a.ckpt"}
            },
            "presets": {"portrait": {"steps": 30}},
            "preset_info": {"portrait": {"label": "Portrait 2:3", "description": "Tall canvas", "kind": "layout"}},
            "outputs": [{"node": "9", "kind": "image"}]
        }"#,
    )
    .unwrap();
    fs::write(dir.join("checkpoints.toml"), PROFILES).unwrap();
}

fn checkpoints(names: &[&str]) -> Value {
    json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [names, {}]}}}})
}

#[tokio::test]
async fn list_checkpoints_names_exact_profile_matches_and_pages() {
    let (api, _llm) = start().await;
    install_pack(&api);
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&[
        "a.ckpt",
        "B.safetensors",
        "b.safetensors",
        "c.ckpt",
    ]));

    let result = invoke(&api, "list_checkpoints", json!({})).await.unwrap();
    let rows = result["checkpoints"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["name"] == name).unwrap();
    assert_eq!(row("b.safetensors")["profiles"][0]["id"], "base");
    assert_eq!(row("b.safetensors")["profiles"][0]["pack"], "chars");
    assert!(row("b.safetensors")["profiles"][0]["revision"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(row("B.safetensors")["profiles"], json!([]), "case differs");
    assert_eq!(row("a.ckpt")["profiles"], json!([]));
    assert!(result["profile_note"]
        .as_str()
        .unwrap()
        .contains("no model family is inferred"));
    assert_eq!(result["truncated"], false);

    let first = invoke(&api, "list_checkpoints", json!({"limit": 2}))
        .await
        .unwrap();
    assert_eq!(first["checkpoints"].as_array().unwrap().len(), 2);
    assert_eq!(first["truncated"], true);
    assert!(first["truncation_note"]
        .as_str()
        .unwrap()
        .contains("next_cursor"));
    let second = invoke(
        &api,
        "list_checkpoints",
        json!({"limit": 2, "cursor": first["next_cursor"]}),
    )
    .await
    .unwrap();
    let names: Vec<_> = first["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["checkpoints"].as_array().unwrap())
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["B.safetensors", "a.ckpt", "b.safetensors", "c.ckpt"]
    );
    assert_eq!(second["truncated"], false);
    assert!(invoke(&api, "list_checkpoints", json!({"limit": 51}))
        .await
        .is_err());
}

#[tokio::test]
async fn list_checkpoints_is_an_error_when_comfyui_is_unreachable() {
    let (api, _llm) = start().await;
    api.fake
        .object_info_unavailable
        .store(true, Ordering::SeqCst);
    let error = invoke(&api, "list_checkpoints", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(error, genaimage_core::HarnessError::ComfyUi(_)),
        "{error}"
    );
    *api.fake.object_info_override.lock().unwrap() = Some(json!({}));
    api.fake
        .object_info_unavailable
        .store(false, Ordering::SeqCst);
    let error = invoke(&api, "list_checkpoints", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ckpt_name"), "{error}");
}

#[tokio::test]
async fn inspect_workflow_shows_layouts_profiles_and_exact_numbers() {
    let (api, _llm) = start().await;
    install_pack(&api);
    let result = invoke(
        &api,
        "inspect_workflow",
        json!({"workflow_id": "chars-flow"}),
    )
    .await
    .unwrap();
    let revision = api
        .core
        .load_workflow_by_id("chars-flow")
        .unwrap()
        .unwrap()
        .revision();
    assert_eq!(result["revision"], revision);
    assert_eq!(result["preset_info"]["portrait"]["label"], "Portrait 2:3");
    assert_eq!(result["preset_info"]["portrait"]["kind"], "layout");
    assert_eq!(result["checkpoint_parameter"], "checkpoint");
    let profile = &result["checkpoint_profiles"][0];
    assert_eq!(profile["id"], "base");
    assert_eq!(profile["label"], "Base");
    assert_eq!(profile["checkpoints"], json!(["b.safetensors"]));
    assert_eq!(profile["ignored"], json!(["unrelated"]));
    assert!(profile["revision"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(profile["values"]["cfg"].to_string(), "2.0");
    assert_eq!(
        result["parameters"]["seed"]["default"].to_string(),
        "9007199254740993"
    );
    assert_eq!(result["parameters"]["shift"]["default"].to_string(), "-0.0");

    install_workflow(&api);
    let plain = invoke(
        &api,
        "inspect_workflow",
        json!({"workflow_id": "contract-workflow"}),
    )
    .await
    .unwrap();
    assert_eq!(plain["checkpoint_profiles"], json!([]));
    assert_eq!(plain["preset_info"], json!({}));
}

async fn api_json(api: &ApiHarness, path: &str, body: &Value) -> Value {
    let response = api
        .request(Method::POST, path)
        .json(body)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.status()
    );
    response.json().await.unwrap()
}

async fn completed_run(
    api: &ApiHarness,
    number: u32,
    character: Option<Value>,
) -> (String, String) {
    api.fake.complete.store(false, Ordering::SeqCst);
    let mut body =
        json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 99}});
    if let Some(character) = character {
        body["character"] = character;
    }
    let response = api
        .request(Method::POST, "/api/v1/runs")
        .header("Idempotency-Key", format!("read-{number}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let run: Value = response.json().await.unwrap();
    let run_id = run["id"].as_str().unwrap().to_owned();
    api.fake.complete.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if api.core.reconcile(&run_id).await.unwrap().run.status
            == genaimage_core::RunStatus::Completed
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let outputs = invoke(api, "list_outputs", json!({"run_id": run_id}))
        .await
        .unwrap();
    let output = outputs["outputs"][0]["output_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (run_id, output)
}

#[tokio::test]
async fn runs_and_outputs_are_bounded_filtered_and_metadata_only() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let character = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": spec()}),
    )
    .await;
    let id = character["character"]["id"].as_str().unwrap().to_owned();
    let link = json!({"id": id, "spec_version": 1});
    let (linked, linked_output) = completed_run(&api, 1, Some(link.clone())).await;
    let (plain, _) = completed_run(&api, 2, None).await;
    let (_, _) = completed_run(&api, 3, Some(link)).await;

    let all = invoke(&api, "list_runs", json!({})).await.unwrap();
    assert_eq!(all["runs"].as_array().unwrap().len(), 3);
    assert_eq!(all["runs"][0]["status"], "completed");
    assert_eq!(all["runs"][0]["workflow_id"], "contract-workflow");
    assert_eq!(all["runs"][0]["output_count"], 1);
    assert_eq!(all["runs"][2]["id"], linked, "newest first");
    assert_eq!(
        all["runs"][2]["character"],
        json!({"id": id, "spec_version": 1})
    );
    assert_eq!(all["runs"][1]["id"], plain);
    assert_eq!(all["runs"][1]["character"], Value::Null);
    assert_eq!(all["truncated"], false);

    let page = invoke(&api, "list_runs", json!({"limit": 2}))
        .await
        .unwrap();
    assert_eq!(page["runs"].as_array().unwrap().len(), 2);
    assert_eq!(page["truncated"], true);
    assert!(page["truncation_note"].is_string());
    let rest = invoke(
        &api,
        "list_runs",
        json!({"limit": 2, "cursor": page["next_cursor"]}),
    )
    .await
    .unwrap();
    assert_eq!(rest["runs"][0]["id"], linked);

    let by_character = invoke(
        &api,
        "list_runs",
        json!({"character_id": id, "spec_version": 1}),
    )
    .await
    .unwrap();
    assert_eq!(by_character["runs"].as_array().unwrap().len(), 2);
    let by_workflow = invoke(&api, "list_runs", json!({"workflow": "nothing"}))
        .await
        .unwrap();
    assert_eq!(by_workflow["runs"], json!([]));
    let by_status = invoke(&api, "list_runs", json!({"status": "failed"}))
        .await
        .unwrap();
    assert_eq!(by_status["runs"], json!([]));
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 51}),
        json!({"character_id": id, "workflow": "contract-workflow"}),
        json!({"spec_version": 1}),
        json!({"status": "bogus"}),
        json!({"extra": 1}),
    ] {
        assert!(
            invoke(&api, "list_runs", bad.clone()).await.is_err(),
            "{bad}"
        );
    }

    let outputs = invoke(&api, "list_outputs", json!({"character_id": id}))
        .await
        .unwrap();
    assert_eq!(outputs["outputs"].as_array().unwrap().len(), 2);
    let text = outputs.to_string();
    assert!(!text.contains("local_path") && !text.contains("bytes\""));
    assert_eq!(
        invoke(&api, "list_outputs", json!({"run_id": linked}))
            .await
            .unwrap()["outputs"][0]["output_id"],
        linked_output
    );
    let paged = invoke(&api, "list_outputs", json!({"limit": 1}))
        .await
        .unwrap();
    assert_eq!(paged["truncated"], true);
    assert!(invoke(
        &api,
        "list_outputs",
        json!({"run_id": linked, "character_id": id})
    )
    .await
    .is_err());
}

#[tokio::test]
async fn get_character_reports_spec_references_and_decisions() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let spec = json!({
        "summary": "A lighthouse keeper", "elements": [], "palette": [],
        "views": ["front"], "style_notes": "Gouache"
    });
    let created = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": spec}),
    )
    .await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();
    api_json(&api, "/api/v1/characters", &json!({"name": "Other"})).await;

    let listed = invoke(&api, "list_characters", json!({})).await.unwrap();
    assert_eq!(listed["characters"].as_array().unwrap().len(), 2);
    assert_eq!(listed["characters"][1]["name"], "Mara");
    let paged = invoke(&api, "list_characters", json!({"limit": 1}))
        .await
        .unwrap();
    assert_eq!(paged["truncated"], true);

    let upload = api
        .request(Method::POST, "/api/v1/uploads")
        .multipart(
            multipart::Form::new().part(
                "file",
                multipart::Part::bytes(PNG.to_vec())
                    .file_name("s.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    let upload: Value = upload.json().await.unwrap();
    api_json(
        &api,
        &format!("/api/v1/characters/{id}/references"),
        &json!({"upload_id": upload["upload_id"], "role": "sketch", "note": "pose"}),
    )
    .await;
    let (_, output) = completed_run(&api, 1, Some(json!({"id": id, "spec_version": 1}))).await;
    api_json(
        &api,
        &format!("/api/v1/characters/{id}/decisions"),
        &json!({"output_id": output, "spec_version": 1, "decision": "approved"}),
    )
    .await;

    let result = invoke(&api, "get_character", json!({"character_id": id}))
        .await
        .unwrap();
    assert_eq!(result["character"]["name"], "Mara");
    assert_eq!(result["latest_spec_version"]["version"], 1);
    assert_eq!(
        result["latest_spec_version"]["spec"]["summary"],
        "A lighthouse keeper"
    );
    assert_eq!(result["references"]["items"][0]["role"], "sketch");
    assert_eq!(
        result["references"]["items"][0]["upload_id"],
        upload["upload_id"]
    );
    assert!(result["references"]["note"]
        .as_str()
        .unwrap()
        .contains("chosen by the user"));
    assert_eq!(result["decisions"]["approved"]["count"], 1);
    assert_eq!(result["decisions"]["rejected"]["count"], 0);
    assert_eq!(
        result["decisions"]["recent_approved"][0]["output_id"],
        output
    );
    assert!(
        invoke(&api, "get_character", json!({"character_id": "missing"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn list_annotations_labels_vision_reviews_as_model_opinion() {
    let (api, llm) = start().await;
    install_workflow(&api);
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    *api.fake.output_bytes.lock().unwrap() = Some(png.into_inner());
    let (_, output) = completed_run(&api, 1, None).await;

    let empty = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    assert_eq!(empty["annotations"], json!([]));
    assert_eq!(empty["source"], "model_opinion");

    llm.set_scripts([
        tool_script("review", "review_image", json!({"output_id": output})),
        text_script(
            r#"{"caption":"A generated image","adherence_critique":"Matches.","tags":["generated"]}"#,
        ),
        text_script("reviewed"),
    ]);
    let client = Client::with_base_url(&api.origin).unwrap();
    let conversation = client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Review".into(),
        })
        .await
        .unwrap();
    let mut stream = client
        .send_assistant_message(
            &conversation.id,
            &SendAssistantMessageRequest {
                content: vec![AssistantChatContent::Text("review".into())],
            },
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        if let AssistantStreamEvent::Done(_) = event.unwrap() {
            break;
        }
    }
    let result = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    let annotation = &result["annotations"][0];
    assert_eq!(annotation["kind"], "vision_review");
    assert_eq!(annotation["model"], VISION_MODEL);
    assert_eq!(annotation["caption"], "A generated image");
    assert!(annotation.get("raw_response").is_none() && annotation.get("prompt").is_none());
    assert!(result["note"]
        .as_str()
        .unwrap()
        .contains("not user approval"));
    assert!(invoke(
        &api,
        "list_annotations",
        json!({"output_id": "run:missing"})
    )
    .await
    .is_err());
}

#[tokio::test]
async fn a_chat_only_conversation_reads_checkpoints_workflow_and_character() {
    let (api, llm) = start().await;
    install_pack(&api);
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&["b.safetensors"]));
    let created = api_json(&api, "/api/v1/characters", &json!({"name": "Mara"})).await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();
    let client = Client::with_base_url(&api.origin).unwrap();
    let conversation = client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Read".into(),
        })
        .await
        .unwrap();
    let steps = [
        ("list_checkpoints", json!({})),
        ("inspect_workflow", json!({"workflow_id": "chars-flow"})),
        ("get_character", json!({"character_id": id})),
    ];
    for (index, (name, arguments)) in steps.into_iter().enumerate() {
        llm.set_scripts([
            tool_script(&format!("call{index}"), name, arguments),
            text_script("done"),
        ]);
        let mut stream = client
            .send_assistant_message(
                &conversation.id,
                &SendAssistantMessageRequest {
                    content: vec![AssistantChatContent::Text(name.into())],
                },
            )
            .await
            .unwrap();
        let mut result = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                AssistantStreamEvent::ToolResult(event) => {
                    assert!(event.ok, "{name}: {event:?}");
                    result = event.result;
                }
                AssistantStreamEvent::Done(_) => break,
                AssistantStreamEvent::Error(event) => panic!("{event:?}"),
                _ => {}
            }
        }
        let result = result.unwrap_or_else(|| panic!("{name} returned nothing"));
        match name {
            "list_checkpoints" => assert_eq!(result["checkpoints"][0]["profiles"][0]["id"], "base"),
            "inspect_workflow" => assert_eq!(result["preset_info"]["portrait"]["kind"], "layout"),
            _ => assert_eq!(result["character"]["id"], id),
        }
    }
}
