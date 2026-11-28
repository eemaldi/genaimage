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
    assert_eq!(result["latest_spec"]["version"], 1);
    assert_eq!(result["latest_spec"]["element_count"], 0);
    assert_eq!(result["latest_spec"]["content_included"], false);
    assert!(result["latest_spec"]["spec_bytes"].as_u64().unwrap() > 0);
    assert!(result["latest_spec"].get("spec").is_none());
    assert!(!result.to_string().contains("lighthouse keeper"));
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
    assert_eq!(annotation["label"], "model opinion");
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

const ROW_TABLES: [&str; 8] = [
    "runs",
    "outputs",
    "output_annotations",
    "characters",
    "character_spec_versions",
    "character_output_decisions",
    "character_references",
    "assistant_tool_confirmations",
];

fn row_counts(api: &ApiHarness) -> Vec<(String, i64)> {
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    let mut counts: Vec<_> = ROW_TABLES
        .iter()
        .map(|table| {
            let count = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table.to_string(), count)
        })
        .collect();
    let uploads = fs::read_dir(&api.core.config().uploads)
        .map(|entries| entries.count() as i64)
        .unwrap_or(0);
    counts.push(("uploads".into(), uploads));
    counts
}

fn clone_runs(api: &ApiHarness, template: &str, count: usize, error: &str) {
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('runs')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let selected: Vec<String> = columns
        .iter()
        .map(|column| match column.as_str() {
            "id" => "?1".into(),
            "prompt_id" => "?2".into(),
            "created_at" => "?3".into(),
            "status" => "'failed'".into(),
            "error" => "?4".into(),
            other => other.to_owned(),
        })
        .collect();
    let sql = format!(
        "INSERT INTO runs ({}) SELECT {} FROM runs WHERE id = ?5",
        columns.join(", "),
        selected.join(", ")
    );
    for index in 0..count {
        db.execute(
            &sql,
            rusqlite::params![
                format!("clone-run-{index:03}"),
                format!("clone-prompt-{index:03}"),
                format!("2031-01-01T00:{:02}:{:02}+00:00", index / 60, index % 60),
                error,
                template
            ],
        )
        .unwrap();
    }
}

async fn walk(api: &ApiHarness, tool: &str, key: &str, id_key: &str, base: Value) -> Vec<String> {
    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let mut arguments = base.clone();
        if !cursor.is_null() {
            arguments["cursor"] = cursor.clone();
        }
        let page = invoke(api, tool, arguments).await.unwrap();
        assert!(
            page.get("truncation_notes").is_none(),
            "{tool}: generic trimming applied: {page}"
        );
        assert!(serde_json::to_vec(&page).unwrap().len() <= 32 * 1024);
        let rows = page[key].as_array().unwrap();
        assert!(!rows.is_empty(), "{tool}: an empty page must be the last");
        seen.extend(
            rows.iter()
                .map(|row| row[id_key].as_str().unwrap().to_owned()),
        );
        if page["next_cursor"].is_null() {
            assert_eq!(page["truncated"], false);
            return seen;
        }
        assert_eq!(page["truncated"], true);
        assert!(page["truncation_note"].is_string());
        cursor = page["next_cursor"].clone();
    }
}

fn assert_each_once(seen: &[String], expected: usize) {
    let mut unique = seen.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "a row appeared twice");
    assert_eq!(seen.len(), expected, "a row was skipped");
}

#[tokio::test]
async fn long_run_pages_are_shortened_whole_and_cursors_visit_every_row_once() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let character = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": spec()}),
    )
    .await;
    let id = character["character"]["id"].as_str().unwrap().to_owned();
    let (template, _) = completed_run(&api, 1, Some(json!({"id": id, "spec_version": 1}))).await;
    clone_runs(&api, &template, 60, &"é".repeat(3_000));

    let first = invoke(&api, "list_runs", json!({"limit": 50}))
        .await
        .unwrap();
    let returned = first["runs"].as_array().unwrap().len();
    assert!(returned < 50, "the maximal page must be shortened");
    assert_eq!(first["truncated"], true);
    assert!(first["truncation_note"]
        .as_str()
        .unwrap()
        .contains("shortened to fit"));
    assert_eq!(first["runs"][0]["id"], "clone-run-059");
    let error = first["runs"][0]["error"].as_str().unwrap();
    assert!(
        error.ends_with("…(truncated)") && error.len() < 330,
        "{error}"
    );
    assert!(first["next_cursor"].is_string());

    let all = walk(&api, "list_runs", "runs", "id", json!({"limit": 50})).await;
    assert_each_once(&all, 61);
    let small = walk(&api, "list_runs", "runs", "id", json!({"limit": 7})).await;
    assert_eq!(small, all);
    let by_character = walk(
        &api,
        "list_runs",
        "runs",
        "id",
        json!({"character_id": id, "spec_version": 1, "limit": 50}),
    )
    .await;
    assert_each_once(&by_character, 61);
    let by_workflow = walk(
        &api,
        "list_runs",
        "runs",
        "id",
        json!({"workflow": "contract-workflow", "status": "failed"}),
    )
    .await;
    assert_each_once(&by_workflow, 60);
}

#[tokio::test]
async fn output_and_character_pages_follow_cursors_exactly_once() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let character = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": spec()}),
    )
    .await;
    let id = character["character"]["id"].as_str().unwrap().to_owned();
    for number in 1..=3 {
        completed_run(&api, number, Some(json!({"id": id, "spec_version": 1}))).await;
    }
    let by_character = walk(
        &api,
        "list_outputs",
        "outputs",
        "output_id",
        json!({"character_id": id, "spec_version": 1, "limit": 1}),
    )
    .await;
    assert_each_once(&by_character, 3);
    let everything = walk(
        &api,
        "list_outputs",
        "outputs",
        "output_id",
        json!({"limit": 2}),
    )
    .await;
    assert_eq!(everything.len(), 3);
    assert_each_once(&everything, 3);

    for name in ["Two", "Three", "Four", "Five"] {
        api_json(&api, "/api/v1/characters", &json!({"name": name})).await;
    }
    let characters = walk(
        &api,
        "list_characters",
        "characters",
        "id",
        json!({"limit": 2}),
    )
    .await;
    assert_each_once(&characters, 5);
}

#[tokio::test]
async fn long_checkpoint_pages_keep_names_whole_and_follow_cursors() {
    let (api, _llm) = start().await;
    install_pack(&api);
    let names: Vec<String> = (0..50)
        .map(|index| format!("{index:02}-{}.safetensors", "n".repeat(1_500)))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&refs));
    let first = invoke(&api, "list_checkpoints", json!({"limit": 50}))
        .await
        .unwrap();
    assert!(first["checkpoints"].as_array().unwrap().len() < 50);
    assert!(first["truncation_note"]
        .as_str()
        .unwrap()
        .contains("shortened to fit"));
    assert_eq!(first["checkpoints"][0]["name"], names[0]);
    let seen = walk(
        &api,
        "list_checkpoints",
        "checkpoints",
        "name",
        json!({"limit": 50}),
    )
    .await;
    assert_each_once(&seen, 50);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "names are returned whole and in order");

    let too_long = format!("{}.safetensors", "x".repeat(3_000));
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&[too_long.as_str()]));
    let error = invoke(&api, "list_checkpoints", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("never shortened"), "{error}");
}

#[tokio::test]
async fn a_malformed_profile_file_is_reported_not_shown_as_no_profile() {
    let (api, _llm) = start().await;
    install_pack(&api);
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&["b.safetensors"]));
    let healthy = invoke(&api, "list_checkpoints", json!({})).await.unwrap();
    assert!(healthy.get("profile_pack_problems").is_none());
    let dir = api.core.config().packs[0].join("chars");
    fs::write(dir.join("checkpoints.toml"), "[[profiles]\nid = ").unwrap();

    let listed = invoke(&api, "list_checkpoints", json!({})).await.unwrap();
    assert_eq!(listed["checkpoints"][0]["profiles"], json!([]));
    let problems = &listed["profile_pack_problems"];
    assert!(problems["count"].as_u64().unwrap() >= 1, "{listed}");
    assert!(problems["messages"][0]
        .as_str()
        .unwrap()
        .contains("checkpoints.toml"));
    assert!(listed["profile_note"]
        .as_str()
        .unwrap()
        .contains("profile_pack_problems"));
    let inspected = invoke(
        &api,
        "inspect_workflow",
        json!({"workflow_id": "chars-flow"}),
    )
    .await
    .unwrap();
    assert_eq!(inspected["checkpoint_profiles"], json!([]));
    assert!(
        inspected["profile_pack_problems"]["count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(inspected["profile_note"].is_string());
}

fn big_spec(elements: usize) -> Value {
    json!({
        "summary": "s".repeat(2_000),
        "style_notes": "n".repeat(4_000),
        "palette": [{"name": "Teal", "hex": "#008080", "role": "primary"}],
        "views": ["front", "side"],
        "elements": (0..elements).map(|index| json!({
            "id": format!("el-{index:02}"),
            "category": "clothing",
            "description": format!("{index:02}{}", "d".repeat(998)),
            "requirement": "mandatory"
        })).collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn get_character_spec_pages_whole_elements_and_never_trims() {
    let (api, _llm) = start().await;
    let spec = big_spec(50);
    let created = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": spec}),
    )
    .await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();

    let summary = invoke(&api, "get_character", json!({"character_id": id}))
        .await
        .unwrap();
    assert!(summary.get("truncation_notes").is_none(), "{summary}");
    assert_eq!(summary["latest_spec"]["element_count"], 50);
    assert!(summary["latest_spec"]["spec_bytes"].as_u64().unwrap() > 50_000);
    assert!(!summary.to_string().contains("dddd"));

    let mut elements = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let mut arguments = json!({"character_id": id});
        if !cursor.is_null() {
            arguments["cursor"] = cursor.clone();
        }
        let page = invoke(&api, "get_character_spec", arguments).await.unwrap();
        pages += 1;
        for key in ["truncated", "truncation_notes", "truncation_note"] {
            assert!(page.get(key).is_none(), "{key}: {page}");
        }
        assert!(serde_json::to_vec(&page).unwrap().len() <= 26 * 1024);
        assert_eq!(page["version"], 1);
        assert_eq!(page["elements_total"], 50);
        if pages == 1 {
            assert_eq!(page["summary"], spec["summary"]);
            assert_eq!(page["style_notes"], spec["style_notes"]);
            assert_eq!(page["palette"], spec["palette"]);
            assert_eq!(page["views"], spec["views"]);
        } else {
            assert!(page.get("summary").is_none() && page.get("style_notes").is_none());
        }
        elements.extend(page["elements"].as_array().unwrap().clone());
        if page["next_cursor"].is_null() {
            assert_eq!(page["complete"], true);
            break;
        }
        assert_eq!(page["complete"], false);
        cursor = page["next_cursor"].clone();
    }
    assert!(pages >= 3);
    assert_eq!(Value::Array(elements), spec["elements"]);

    let pinned = invoke(
        &api,
        "get_character_spec",
        json!({"character_id": id, "version": 1}),
    )
    .await
    .unwrap();
    assert_eq!(pinned["version"], 1);
    for bad in [
        json!({"character_id": id, "version": 2}),
        json!({"character_id": id, "version": 2, "cursor": cursor_of(&pinned)}),
        json!({"character_id": id, "cursor": "nonsense"}),
        json!({"character_id": id, "cursor": "spec.v1.e999"}),
        json!({"character_id": id, "cursor": "spec.v1.e50"}),
        json!({"character_id": id, "cursor": "spec.v1.e0"}),
        json!({"character_id": id, "cursor": "spec.v+1.e3"}),
        json!({"character_id": "missing"}),
        json!({"character_id": id, "extra": 1}),
    ] {
        assert!(
            invoke(&api, "get_character_spec", bad.clone())
                .await
                .is_err(),
            "{bad}"
        );
    }
    let forged = invoke(
        &api,
        "get_character_spec",
        json!({"character_id": id, "cursor": "spec.v1.e50"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(forged.contains("outside this spec version"), "{forged}");
    let bare = api_json(&api, "/api/v1/characters", &json!({"name": "Bare"})).await;
    let bare = bare["character"]["id"].as_str().unwrap().to_owned();
    let error = invoke(&api, "get_character_spec", json!({"character_id": bare}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no spec version"), "{error}");
}

fn cursor_of(page: &Value) -> Value {
    page["next_cursor"].clone()
}

#[tokio::test]
async fn annotations_over_the_limit_say_they_are_truncated() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let (run, output) = completed_run(&api, 1, None).await;
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    for index in 0..12 {
        db.execute(
            "INSERT INTO output_annotations (output_id, run_id, kind, model, created_at, caption, critique, tags_json, prompt, raw_response)
             VALUES (?1, ?2, 'vision_review', 'm', ?3, ?4, 'critique', '[]', 'p', 'r')",
            rusqlite::params![
                output,
                run,
                format!("2031-01-01T00:00:{index:02}+00:00"),
                "c".repeat(3_000)
            ],
        )
        .unwrap();
    }
    drop(db);
    let result = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    assert_eq!(result["truncated"], true);
    assert!(result["truncation_note"]
        .as_str()
        .unwrap()
        .contains("not the complete list"));
    let rows = result["annotations"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.len() <= 10);
    assert!(rows[0]["caption"]
        .as_str()
        .unwrap()
        .ends_with("…(truncated)"));
    assert!(result.get("truncation_notes").is_none(), "{result}");
}

fn spec_review(items: usize, note_bytes: usize) -> genaimage_core::SpecReview {
    use genaimage_core::{characters::ElementCategory, characters::Requirement};
    genaimage_core::SpecReview {
        character_id: "char-1".into(),
        spec_version: 3,
        checklist: (0..items)
            .map(|index| genaimage_core::SpecReviewItem {
                id: format!("element-{index}"),
                category: ElementCategory::Colour,
                requirement: Requirement::Mandatory,
                verdict: genaimage_core::SpecVerdict::Deviation,
                note: "n".repeat(note_bytes),
            })
            .collect(),
        dropped: vec!["element 'x' dropped: unknown id".into()],
        summary: "One deviation".into(),
        label: "model opinion".into(),
    }
}

fn insert_annotation(
    api: &ApiHarness,
    run: &str,
    output: &str,
    kind: &str,
    second: u32,
    detail: Option<&genaimage_core::SpecReview>,
) {
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    db.execute(
        "INSERT INTO output_annotations (output_id, run_id, kind, model, created_at, caption, critique, tags_json, prompt, raw_response, detail_json)
         VALUES (?1, ?2, ?3, 'm', ?4, 'caption', 'critique', '[]', 'p', 'r', ?5)",
        rusqlite::params![
            output,
            run,
            kind,
            format!("2031-01-01T00:00:{second:02}+00:00"),
            detail.map(|review| serde_json::to_string(review).unwrap())
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn spec_reviews_are_projected_whole_beside_vision_reviews() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let (run, output) = completed_run(&api, 1, None).await;
    let review = spec_review(2, 20);
    insert_annotation(&api, &run, &output, "vision_review", 1, None);
    insert_annotation(&api, &run, &output, "spec_review", 2, Some(&review));
    let result = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    let rows = result["annotations"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{result}");
    assert_eq!(rows[0]["kind"], "spec_review");
    assert_eq!(
        rows[0]["label"],
        "model opinion (spec review), not approval"
    );
    assert_eq!(
        rows[0]["spec_review"],
        json!({
            "character_id": "char-1",
            "spec_version": 3,
            "checklist": [
                {"id": "element-0", "requirement": "mandatory", "verdict": "deviation", "note": "n".repeat(20)},
                {"id": "element-1", "requirement": "mandatory", "verdict": "deviation", "note": "n".repeat(20)}
            ],
            "dropped": ["element 'x' dropped: unknown id"],
            "summary": "One deviation",
            "label": "model opinion"
        })
    );
    assert_eq!(rows[1]["kind"], "vision_review");
    assert_eq!(rows[1]["label"], "model opinion");
    assert!(rows[1].get("spec_review").is_none());
    assert_eq!(result["truncated"], false);
}

async fn annotation_pages(api: &ApiHarness, id: &Value) -> Vec<Value> {
    let mut pages: Vec<Value> = Vec::new();
    let mut arguments = json!({"annotation_id": id});
    loop {
        let page = invoke(api, "get_annotation", arguments.clone())
            .await
            .unwrap();
        assert!(page.get("truncated").is_none(), "{page}");
        assert!(serde_json::to_vec(&page).unwrap().len() <= 32 * 1024);
        let next = page["next_cursor"].clone();
        assert_eq!(page["complete"], next.is_null(), "{page}");
        pages.push(page);
        if next.is_null() {
            return pages;
        }
        arguments = json!({"annotation_id": id, "cursor": next});
    }
}

#[tokio::test]
async fn an_oversized_spec_review_is_a_summary_row_and_reads_whole_through_get_annotation() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let (run, output) = completed_run(&api, 1, None).await;
    insert_annotation(
        &api,
        &run,
        &output,
        "spec_review",
        1,
        Some(&spec_review(64, 500)),
    );
    insert_annotation(&api, &run, &output, "vision_review", 2, None);

    let listed = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    let rows = listed["annotations"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed}");
    assert_eq!(listed["truncated"], false);
    assert_eq!(rows[0]["kind"], "vision_review");
    assert_eq!(rows[0]["complete"], true);
    let summary = &rows[1];
    assert_eq!(summary["kind"], "spec_review");
    assert_eq!(summary["complete"], false);
    assert_eq!(
        summary["label"],
        "model opinion (spec review), not approval"
    );
    assert_eq!(
        summary["spec_review"],
        json!({
            "character_id": "char-1",
            "spec_version": 3,
            "checklist_total": 64,
            "verdict_counts": {"met": 0, "deviation": 64, "unclear": 0},
            "dropped": 1,
            "summary": "One deviation",
            "label": "model opinion"
        })
    );
    assert!(summary["spec_review"].get("checklist").is_none());
    assert_eq!(summary["read_with"]["tool"], "get_annotation");
    assert!(summary["note"]
        .as_str()
        .unwrap()
        .contains("complete is true"));

    let pages = annotation_pages(&api, &summary["id"]).await;
    assert!(pages.len() > 1, "a maximal review needs several pages");
    let mut seen = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(page["annotation_id"], summary["id"]);
        assert_eq!(page["checklist_total"], 64);
        assert_eq!(
            page.get("spec_review").is_some(),
            index == 0,
            "only the first page carries the header"
        );
        for item in page["checklist"].as_array().unwrap() {
            seen.push(item["id"].as_str().unwrap().to_owned());
            assert_eq!(item["note"].as_str().unwrap().len(), 500);
        }
        assert_eq!(page["complete"], index == pages.len() - 1);
    }
    assert_eq!(
        seen,
        (0..64)
            .map(|index| format!("element-{index}"))
            .collect::<Vec<_>>()
    );
    let header = &pages[0]["spec_review"];
    assert_eq!(header["verdict_counts"]["deviation"], 64);
    assert_eq!(
        header["dropped"],
        json!(["element 'x' dropped: unknown id"])
    );
    assert_eq!(header["summary"], "One deviation");
}

#[tokio::test]
async fn get_annotation_rejects_forged_cursors_and_unknown_ids() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let (run, output) = completed_run(&api, 1, None).await;
    insert_annotation(
        &api,
        &run,
        &output,
        "spec_review",
        1,
        Some(&spec_review(64, 500)),
    );
    insert_annotation(&api, &run, &output, "vision_review", 2, None);
    let listed = invoke(&api, "list_annotations", json!({"output_id": output}))
        .await
        .unwrap();
    let vision = listed["annotations"][0]["id"].clone();
    let review = listed["annotations"][1]["id"].clone();
    let pages = annotation_pages(&api, &review).await;
    let last = pages.last().unwrap();
    assert_eq!(last["complete"], true);

    for (cursor, expected) in [
        (
            format!("ann.{review}.i64"),
            "outside this annotation checklist",
        ),
        (
            format!("ann.{review}.i65"),
            "outside this annotation checklist",
        ),
        (
            format!("ann.{review}.i0"),
            "outside this annotation checklist",
        ),
        (format!("ann.{vision}.i3"), "belongs to another annotation"),
        (format!("ann.{review}"), "not a get_annotation cursor"),
        (format!("ann.{review}.i+3"), "not a get_annotation cursor"),
        ("spec.v1.e3".to_owned(), "not a get_annotation cursor"),
    ] {
        let error = invoke(
            &api,
            "get_annotation",
            json!({"annotation_id": review, "cursor": cursor}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{cursor}: {error}");
    }
    let error = invoke(
        &api,
        "get_annotation",
        json!({"annotation_id": vision, "cursor": format!("ann.{vision}.i1")}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("no checklist to page"), "{error}");
    let error = invoke(&api, "get_annotation", json!({"annotation_id": 987_654}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no annotation has this id"), "{error}");

    let whole = invoke(&api, "get_annotation", json!({"annotation_id": vision}))
        .await
        .unwrap();
    assert_eq!(whole["complete"], true);
    assert_eq!(whole["caption"], "caption");
    assert_eq!(whole["label"], "model opinion");
}

#[tokio::test]
async fn list_annotations_pages_with_cursors_and_visits_every_annotation_once() {
    let (api, _llm) = start().await;
    install_workflow(&api);
    let (run, output) = completed_run(&api, 1, None).await;
    for second in 1..=5 {
        insert_annotation(&api, &run, &output, "vision_review", second, None);
    }
    let mut ids = Vec::new();
    let mut arguments = json!({"output_id": output, "limit": 2});
    let mut sizes = Vec::new();
    loop {
        let page = invoke(&api, "list_annotations", arguments.clone())
            .await
            .unwrap();
        let rows = page["annotations"].as_array().unwrap();
        sizes.push(rows.len());
        ids.extend(rows.iter().map(|row| row["id"].as_i64().unwrap()));
        assert_eq!(page["truncated"], !page["next_cursor"].is_null());
        if page["next_cursor"].is_null() {
            assert!(page.get("truncation_note").is_none(), "{page}");
            break;
        }
        assert!(page["truncation_note"]
            .as_str()
            .unwrap()
            .contains("next_cursor"));
        arguments["cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(sizes, [2, 2, 1]);
    assert_eq!(ids.len(), 5);
    assert!(
        ids.windows(2).all(|pair| pair[0] > pair[1]),
        "newest first: {ids:?}"
    );

    for cursor in ["ann.99999", "ann.", "ann.+3", "page-2", "ann.1.i1"] {
        let error = invoke(
            &api,
            "list_annotations",
            json!({"output_id": output, "cursor": cursor}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("list_annotations cursor") || error.contains("does not match"),
            "{cursor}: {error}"
        );
    }
}

#[tokio::test]
async fn no_read_tool_writes_anything() {
    let (api, _llm) = start().await;
    install_pack(&api);
    install_workflow(&api);
    let created = api_json(
        &api,
        "/api/v1/characters",
        &json!({"name": "Mara", "spec": big_spec(3)}),
    )
    .await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();
    let (run, output) = completed_run(&api, 1, Some(json!({"id": id, "spec_version": 1}))).await;
    insert_annotation(&api, &run, &output, "vision_review", 1, None);
    *api.fake.object_info_override.lock().unwrap() = Some(checkpoints(&["b.safetensors"]));
    let before = row_counts(&api);

    for (name, arguments) in [
        ("list_workflows", json!({})),
        ("inspect_workflow", json!({"workflow_id": "chars-flow"})),
        ("list_checkpoints", json!({})),
        ("list_runs", json!({})),
        ("list_runs", json!({"character_id": id})),
        ("run_status", json!({"run_id": run})),
        ("list_outputs", json!({})),
        ("list_outputs", json!({"character_id": id})),
        ("get_output", json!({"output_id": output})),
        ("list_characters", json!({})),
        ("get_character", json!({"character_id": id})),
        ("get_character_spec", json!({"character_id": id})),
        ("list_annotations", json!({"output_id": output})),
        ("get_annotation", json!({"annotation_id": 1})),
    ] {
        invoke(&api, name, arguments)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    assert_eq!(row_counts(&api), before);
}
