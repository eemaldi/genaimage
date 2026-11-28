mod common;

use std::{
    collections::BTreeMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use common::{install_workflow, ApiHarness};
use genaimage_api::{
    client::Client, OutputAnnotationListResponse, OutputAnnotationResponse, OutputListResponse,
    OutputResponse,
};
use genaimage_core::{RunRecord, RunRequest, RunStatus};
use genaimage_test_support::{FakeLlmScript, FakeLlmServer};
use reqwest::{header, Method, StatusCode};
use serde_json::{json, Value};

const OUTPUTS: &str = "/api/v1/outputs";

const VISION_MODEL: &str = "vision-model";
const REVIEW_RESPONSE: &str = r#"{"caption":"A lighthouse above the sea","adherence_critique":"The image follows the lighthouse brief.","tags":["coast","lighthouse"]}"#;

fn review_script(response: &str) -> FakeLlmScript {
    FakeLlmScript::text_chunks([response]).with_models_response(json!({
        "object": "list",
        "data": [
            { "id": "text-model", "supports_tools": true, "supports_vision": false },
            { "id": VISION_MODEL, "supports_tools": false, "supports_vision": true }
        ]
    }))
}

fn file_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

async fn completed(api: &ApiHarness, tag: &str) -> RunRecord {
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut workflow: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    workflow["tags"] = json!([tag]);
    std::fs::write(&path, serde_json::to_vec(&workflow).unwrap()).unwrap();
    api.fake.complete.store(false, Ordering::SeqCst);
    let run = api
        .submit_run(RunRequest {
            checkpoint_profile: None,
            checkpoint_profile_revision: None,
            workflow_revision: None,
            character: None,
            workflow: "contract-workflow".into(),
            preset: Some("fast".into()),
            parameters: Default::default(),
            version: None,
        })
        .await
        .unwrap();
    api.fake.complete.store(true, Ordering::SeqCst);
    let run = api.core.reconcile(&run.id).await.unwrap().run;
    assert_eq!(run.status, RunStatus::Completed);
    run
}

async fn completed_review_output(api: &ApiHarness) -> (RunRecord, OutputResponse, Vec<u8>) {
    install_workflow(api);
    let manifest_path = api.core.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["parameters"]["prompt"] = json!({
        "node": "3", "input": "prompt", "kind": "string", "required": true
    });
    manifest["presets"]["fast"]["prompt"] = json!("A lighthouse above the sea at dawn");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let workflow_path = api.core.config().workflows.join("workflow.api.json");
    let mut workflow: Value = serde_json::from_slice(&fs::read(&workflow_path).unwrap()).unwrap();
    workflow["3"]["inputs"]["prompt"] = json!("A lighthouse above the sea at dawn");
    fs::write(
        &workflow_path,
        serde_json::to_vec_pretty(&workflow).unwrap(),
    )
    .unwrap();
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(4, 3)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    *api.fake.output_bytes.lock().unwrap() = Some(png.clone());
    let run = completed(api, "review").await;
    let outputs: OutputListResponse = api.get_json(OUTPUTS).await;
    (run, outputs.outputs.into_iter().next().unwrap(), png)
}

#[tokio::test]
async fn reviews_with_the_pinned_vision_model_and_preserves_provenance() {
    let llm = FakeLlmServer::start(review_script(REVIEW_RESPONSE)).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "text-model"),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
    )
    .await;
    let (run, output, png) = completed_review_output(&api).await;
    let run_dir = PathBuf::from(&run.output_dir);
    let run_before = serde_json::to_vec(&api.core.get_run(&run.id).unwrap()).unwrap();
    let files_before = file_tree(&run_dir);
    let review_path = format!("{OUTPUTS}/{}/review", output.id);

    let refused = api
        .request_with_origin(Method::POST, &review_path, Some("http://evil.example"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(refused, StatusCode::FORBIDDEN).await;
    assert_eq!(llm.chat_request_count(), 0);

    let first = api
        .request(Method::POST, &review_path)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: OutputAnnotationResponse = first.json().await.unwrap();
    assert_eq!(first.output_id, output.id);
    assert_eq!(first.run_id, run.id);
    assert_eq!(first.kind, "vision_review");
    assert_eq!(first.model, VISION_MODEL);
    assert_eq!(first.caption, "A lighthouse above the sea");
    assert_eq!(
        first.adherence_critique,
        "The image follows the lighthouse brief."
    );
    assert_eq!(first.tags, ["coast", "lighthouse"]);
    assert_eq!(first.raw_response, REVIEW_RESPONSE);
    assert!(first.prompt.contains("A lighthouse above the sea at dawn"));

    let requests = llm.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["model"], VISION_MODEL);
    assert_ne!(request["model"], "text-model");
    let contents = request["messages"][0]["content"].as_array().unwrap();
    let text = contents
        .iter()
        .find(|part| part["type"] == "text")
        .expect("review text part")["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("A lighthouse above the sea at dawn"));
    let image_url = contents
        .iter()
        .find(|part| part["type"] == "image_url")
        .expect("review image data URL")["image_url"]["url"]
        .as_str()
        .unwrap();
    assert_eq!(
        image_url,
        format!("data:image/png;base64,{}", STANDARD.encode(&png))
    );

    let client = Client::with_base_url(&api.origin).unwrap();
    let second = client.review_output(&output.id).await.unwrap();
    let annotations = client.output_annotations(&output.id).await.unwrap();
    assert_eq!(annotations.annotations.len(), 2);
    assert_eq!(annotations.annotations[0].id, second.id);
    assert_eq!(annotations.annotations[1].id, first.id);
    assert_eq!(
        serde_json::to_vec(&api.core.get_run(&run.id).unwrap()).unwrap(),
        run_before
    );
    assert_eq!(file_tree(&run_dir), files_before);
    api.shutdown().await;
}

#[tokio::test]
async fn review_without_a_vision_model_returns_a_reasoned_503() {
    let llm = FakeLlmServer::start(review_script(REVIEW_RESPONSE)).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "text-model"),
        ],
    )
    .await;
    let (_, output, _) = completed_review_output(&api).await;

    let response = api
        .request(Method::POST, &format!("{OUTPUTS}/{}/review", output.id))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "urn:genaimage:problem:assistant-unavailable",
    )
    .await;
    assert!(problem.detail.contains("llm.vision_model is empty"));
    assert_eq!(llm.models_request_count(), 0);
    assert_eq!(llm.chat_request_count(), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn review_rejects_a_vision_capability_override_before_the_provider_call() {
    let llm = FakeLlmServer::start(review_script(REVIEW_RESPONSE)).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_extra_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "text-model"),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
        "[llm.capabilities.vision-model]\nvision = false\n",
    )
    .await;
    let (_, output, _) = completed_review_output(&api).await;

    let response = api
        .request(Method::POST, &format!("{OUTPUTS}/{}/review", output.id))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "urn:genaimage:problem:assistant-unavailable",
    )
    .await;
    assert!(problem
        .detail
        .contains("lacks the required Vision capability"));
    assert_eq!(llm.chat_request_count(), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn malformed_review_json_returns_502_without_storing_an_annotation() {
    let llm = FakeLlmServer::start(review_script("not review JSON")).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "text-model"),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
    )
    .await;
    let (_, output, _) = completed_review_output(&api).await;

    let response = api
        .request(Method::POST, &format!("{OUTPUTS}/{}/review", output.id))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_GATEWAY).await;
    let annotations: OutputAnnotationListResponse = api
        .get_json(&format!("{OUTPUTS}/{}/annotations", output.id))
        .await;
    assert!(annotations.annotations.is_empty());
    assert_eq!(llm.chat_request_count(), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn review_of_an_unknown_output_returns_404() {
    let llm = FakeLlmServer::start(review_script(REVIEW_RESPONSE)).await;
    let llm_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm_url.as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "text-model"),
            ("GENAIMAGE_LLM_VISION_MODEL", VISION_MODEL),
        ],
    )
    .await;

    let response = api
        .request(Method::POST, &format!("{OUTPUTS}/missing:1/review"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(llm.models_request_count(), 0);
    assert_eq!(llm.chat_request_count(), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn paginates_and_filters_by_run_workflow_dates_and_archived_tags() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let older = completed(&api, "Gallery").await;
    let newer = completed(&api, "Other").await;
    let first: OutputListResponse = api.get_json(&format!("{OUTPUTS}?limit=1")).await;
    assert_eq!(first.outputs[0].run_id, newer.id);
    let second: OutputListResponse = api
        .get_json(&format!(
            "{OUTPUTS}?limit=1&cursor={}",
            first.next_cursor.unwrap()
        ))
        .await;
    assert!(second.next_cursor.is_none());
    let output = &second.outputs[0];
    assert_eq!(output.run_id, older.id);
    assert_eq!(output.tags, ["Gallery"]);
    let request = api
        .request(Method::GET, OUTPUTS)
        .query(&[
            ("run", older.id.as_str()),
            ("workflow", "contract-workflow"),
            ("tag", "Gallery"),
            ("created_from", &output.created_at.to_rfc3339()),
            ("created_to", &output.created_at.to_rfc3339()),
        ])
        .send()
        .await
        .unwrap();
    let filtered: OutputListResponse = request.error_for_status().unwrap().json().await.unwrap();
    assert_eq!(filtered.outputs.len(), 1);
    assert_eq!(filtered.outputs[0].id, output.id);
    for query in [
        "run=missing",
        "workflow=missing",
        "tag=gallery",
        "created_from=2999-01-01T00:00:00Z",
    ] {
        let empty: OutputListResponse = api.get_json(&format!("{OUTPUTS}?{query}")).await;
        assert!(empty.outputs.is_empty(), "{query}");
    }
    let response = api
        .request(Method::GET, &format!("{OUTPUTS}?cursor=invalid"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    api.shutdown().await;
}

#[tokio::test]
async fn filters_by_run_status_and_fetches_metadata_outside_the_first_page() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let older = completed(&api, "Older").await;
    let newer = completed(&api, "Newer").await;
    let first: OutputListResponse = api
        .get_json(&format!("{OUTPUTS}?limit=1&status=completed"))
        .await;
    assert_eq!(first.outputs[0].run_id, newer.id);
    let older_outputs: OutputListResponse =
        api.get_json(&format!("{OUTPUTS}?run={}", older.id)).await;
    let id = &older_outputs.outputs[0].id;
    let path = format!("{OUTPUTS}/{id}/metadata");
    let metadata: OutputResponse = api.get_json(&path).await;
    assert_eq!(metadata.run_id, older.id);
    assert_eq!(metadata.tags, ["Older"]);
    assert_eq!(metadata.content_type, "image/png");
    let missing = api
        .request(Method::GET, &format!("{OUTPUTS}/missing:1/metadata"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(missing, StatusCode::NOT_FOUND).await;

    // Simulate a terminal failure after the artefact was indexed.
    api.core
        .repository()
        .update_status(&older.id, RunStatus::Failed, Some("Fixture failure"), None)
        .unwrap();
    let default: OutputListResponse = api.get_json(OUTPUTS).await;
    assert_eq!(default.outputs.len(), 1);
    let failed: OutputListResponse = api
        .get_json(&format!("{OUTPUTS}?status=failed&tag=Older"))
        .await;
    assert_eq!(failed.outputs.len(), 1);
    assert_eq!(&failed.outputs[0].id, id);
    let empty: OutputListResponse = api.get_json(&format!("{OUTPUTS}?status=running")).await;
    assert!(empty.outputs.is_empty());
    let invalid = api
        .request(Method::GET, &format!("{OUTPUTS}?status=invalid"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(invalid, StatusCode::BAD_REQUEST).await;
    api.shutdown().await;
}

#[tokio::test]
async fn serves_verified_originals_thumbnails_and_stored_manifests() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(320, 160)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    *api.fake.output_bytes.lock().unwrap() = Some(png.clone());
    let run = completed(&api, "gallery").await;
    let outputs: OutputListResponse = api.get_json(OUTPUTS).await;
    let output = &outputs.outputs[0];
    let path = format!("{OUTPUTS}/{}", output.id);
    let response = api.request(Method::GET, &path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    let etag = format!("\"{}\"", output.sha256);
    assert_eq!(response.headers()[header::ETAG], etag);
    assert_eq!(response.bytes().await.unwrap(), png);
    let response = api
        .request(Method::GET, &path)
        .header(header::IF_NONE_MATCH, format!("\"other\", W/{etag}"))
        .header(header::RANGE, "bytes=1-3")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(response.bytes().await.unwrap().is_empty());
    for (condition, status) in [
        (r#""a,*,b""#, StatusCode::OK),
        (r#"W/"a,*,b""#, StatusCode::OK),
        (r#""other", *"#, StatusCode::OK),
        ("*", StatusCode::NOT_MODIFIED),
    ] {
        let response = api
            .request(Method::GET, &path)
            .header(header::IF_NONE_MATCH, condition)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{condition}");
    }
    let response = api
        .request(Method::GET, &path)
        .header(header::RANGE, "frames=0-5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::CONTENT_RANGE));
    assert_eq!(response.bytes().await.unwrap(), png);
    for (range, start, end) in [
        ("bytes=1-3", 1, 3),
        ("bytes=2-", 2, png.len() - 1),
        ("bytes=-4", png.len() - 4, png.len() - 1),
        ("bytes=1-184467440737095516160", 1, png.len() - 1),
        ("bytes=-184467440737095516160", 0, png.len() - 1),
    ] {
        let response = api
            .request(Method::GET, &path)
            .header(header::RANGE, range)
            .header(header::IF_RANGE, &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes {start}-{end}/{}", png.len())
        );
        assert_eq!(response.bytes().await.unwrap(), &png[start..=end]);
    }
    let response = api
        .request(Method::GET, &path)
        .header(header::RANGE, "bytes=1-3")
        .header(header::IF_RANGE, "\"old\"")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    for range in [
        "bytes=999999-",
        "bytes=0-1,3-4",
        "bytes=-0",
        "bytes=184467440737095516160-",
        "bytes=1-+4",
    ] {
        let response = api
            .request(Method::GET, &path)
            .header(header::RANGE, range)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes */{}", png.len())
        );
        ApiHarness::assert_problem(response, StatusCode::RANGE_NOT_SATISFIABLE).await;
    }
    let thumb_path = format!("{path}/thumbnail?size=128");
    let response = api.request(Method::GET, &thumb_path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/webp");
    let thumb_etag = response.headers()[header::ETAG].clone();
    let thumbnail = response.bytes().await.unwrap();
    let thumbnail =
        image::load_from_memory_with_format(&thumbnail, image::ImageFormat::WebP).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (128, 64));
    let response = api
        .request(Method::GET, &thumb_path)
        .header(header::IF_NONE_MATCH, thumb_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let workspace_data = std::fs::read_dir(api.workspace().join("genaimage/workspaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let cache_dir = workspace_data.join("thumbnails/webp-v1");
    let cached = cache_dir.join(format!("{}-128.webp", output.sha256));
    let truncated = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&cached)
        .unwrap();
    truncated.set_len(12).unwrap();
    drop(truncated);
    let response = api.request(Method::GET, &thumb_path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let regenerated = response.bytes().await.unwrap();
    let decoded =
        image::load_from_memory_with_format(&regenerated, image::ImageFormat::WebP).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (128, 64));
    assert_eq!(std::fs::read(&cached).unwrap(), regenerated);
    for query in ["", "?size=127", "?size=-1", "?size=128&size=512"] {
        let response = api
            .request(Method::GET, &format!("{path}/thumbnail{query}"))
            .send()
            .await
            .unwrap();
        ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    }
    let manifest = PathBuf::from(run.manifest_path.unwrap());
    let manifest_url = format!("/api/v1/runs/{}/manifest", run.id);
    let response = api
        .request(Method::GET, &manifest_url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let manifest_etag = response.headers()[header::ETAG].clone();
    let stored = std::fs::read(&manifest).unwrap();
    assert_eq!(response.bytes().await.unwrap(), stored);
    let response = api
        .request(Method::GET, &manifest_url)
        .header(header::IF_NONE_MATCH, manifest_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    // Even valid JSON for the correct run must not replace the stored manifest.
    std::fs::write(&manifest, [stored.as_slice(), b"\n"].concat()).unwrap();
    let response = api
        .request(Method::GET, &manifest_url)
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    let provenance = api.core.repository().outputs(&run.id).unwrap();
    let mut changed = png;
    changed[0] ^= 1;
    std::fs::write(&provenance[0].local_path, changed).unwrap();
    let response = api
        .request(Method::GET, &path)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    let response = api
        .request(Method::GET, &format!("{path}/thumbnail?size=256"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    for path in [
        format!("{OUTPUTS}/unknown"),
        format!("{OUTPUTS}/unknown/thumbnail?size=128"),
        "/api/v1/runs/unknown/manifest".into(),
    ] {
        let response = api.request(Method::GET, &path).send().await.unwrap();
        ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
    }
    api.shutdown().await;
}

#[tokio::test]
async fn legacy_manifest_must_match_schema_and_is_explicitly_unverified() {
    const INTEGRITY: &str = "x-genaimage-manifest-integrity";
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let run = completed(&api, "gallery").await;
    let manifest = PathBuf::from(run.manifest_path.unwrap());
    let url = format!("/api/v1/runs/{}/manifest", run.id);
    let response = api.request(Method::GET, &url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(INTEGRITY));
    let etag = response.headers()[header::ETAG].clone();
    let stored = response.bytes().await.unwrap();

    {
        let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
        assert_eq!(
            db.execute("DELETE FROM run_media WHERE run_id = ?1", [&run.id])
                .unwrap(),
            1
        );
    }
    let response = api.request(Method::GET, &url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[INTEGRITY], "unverified");
    assert_eq!(response.bytes().await.unwrap(), stored);
    let response = api
        .request(Method::GET, &url)
        .header(header::IF_NONE_MATCH, etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[INTEGRITY], "unverified");

    std::fs::write(&manifest, json!({"run_id": run.id}).to_string()).unwrap();
    let response = api
        .request(Method::GET, &url)
        .header(header::IF_NONE_MATCH, "*")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    api.shutdown().await;
}
