mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::ApiHarness;
use futures_util::StreamExt;
use genaimage_api::{
    CheckpointsResponse, LlmModelSource, LlmModelsResponse, LlmRoleModelState, ModelPull,
    ModelPullListResponse, ModelPullState,
};
use genaimage_test_support::{
    FakeLlmScript, FakeLlmServer, FakeOllamaScript, FakeOllamaServer, FakePullStep,
};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use tokio::time::{sleep, timeout};

const SECRET: &str = "sk-models-secret-4711";
const BOUND: Duration = Duration::from_secs(10);
const LIST: &str = "/api/v1/models/llm";
const PULLS: &str = "/api/v1/models/llm/pulls";

fn gated() -> Vec<FakePullStep> {
    vec![
        FakePullStep::progress("pulling manifest"),
        FakePullStep::layer("sha256:aaa", 5_000_000_000_000_000, 1_250_000_000_000_000),
        FakePullStep::Gate,
        FakePullStep::layer("sha256:aaa", 5_000_000_000_000_000, 5_000_000_000_000_000),
        FakePullStep::success(),
    ]
}

async fn ollama(pull: Vec<FakePullStep>) -> (FakeOllamaServer, ApiHarness) {
    let server = FakeOllamaServer::start(FakeOllamaScript {
        pull,
        ..Default::default()
    })
    .await;
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", &server.base_url()),
            ("GENAIMAGE_LLM_API_KEY", SECRET),
            ("GENAIMAGE_LLM_TEXT_MODEL", "llama3.2:3b"),
            ("GENAIMAGE_LLM_VISION_MODEL", "qwen2.5vl:7b"),
        ],
    )
    .await;
    (server, api)
}

async fn start_pull(api: &ApiHarness, name: &str) -> reqwest::Response {
    api.request(Method::POST, PULLS)
        .json(&json!({ "name": name }))
        .send()
        .await
        .unwrap()
}

async fn started(api: &ApiHarness, name: &str) -> ModelPull {
    let response = start_pull(api, name).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let pull: ModelPull = response.json().await.unwrap();
    assert_eq!(location, format!("{PULLS}/{}", pull.id));
    pull
}

async fn pull_state(api: &ApiHarness, id: &str) -> ModelPull {
    api.get_json(&format!("{PULLS}/{id}")).await
}

async fn wait_for(api: &ApiHarness, id: &str, done: impl Fn(&ModelPull) -> bool) -> ModelPull {
    timeout(BOUND, async {
        loop {
            let pull = pull_state(api, id).await;
            if done(&pull) {
                return pull;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for the pull")
}

async fn finished(api: &ApiHarness, id: &str) -> ModelPull {
    wait_for(api, id, |pull| pull.state != ModelPullState::Running).await
}

async fn sse_frames(response: reqwest::Response) -> Vec<(String, ModelPull)> {
    let mut body = response.bytes_stream();
    let mut text = String::new();
    timeout(BOUND, async {
        while let Some(chunk) = body.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        }
    })
    .await
    .expect("the stream must end after the terminal event");
    text.split("\n\n")
        .filter_map(|frame| {
            let event = frame
                .lines()
                .find_map(|line| line.strip_prefix("event: "))?;
            let data = frame.lines().find_map(|line| line.strip_prefix("data: "))?;
            Some((event.to_owned(), serde_json::from_str(data).unwrap()))
        })
        .collect()
}

#[tokio::test]
async fn listing_through_ollama_flags_the_configured_roles() {
    let (server, api) = ollama(gated()).await;
    let listing: LlmModelsResponse = api.get_json(LIST).await;
    assert_eq!(listing.source, LlmModelSource::Ollama);
    assert!(listing.pull_supported);
    assert_eq!(listing.models[0].name, "llama3.2:3b");
    assert_eq!(listing.models[0].size_bytes, Some(2_019_393_189));
    let states = listing
        .roles
        .iter()
        .map(|role| (role.model.clone().unwrap(), role.state))
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        [
            ("llama3.2:3b".into(), LlmRoleModelState::Present),
            ("qwen2.5vl:7b".into(), LlmRoleModelState::Missing),
        ]
    );
    assert_eq!(server.pull_count(), 0);
}

#[tokio::test]
async fn listing_falls_back_to_v1_models_and_pull_is_unsupported() {
    let fake = FakeLlmServer::start(FakeLlmScript::default().with_models(["test-model"])).await;
    let api = ApiHarness::start_with_config(5, &[("GENAIMAGE_LLM_URL", &fake.base_url())]).await;
    let listing: LlmModelsResponse = api.get_json(LIST).await;
    assert_eq!(listing.source, LlmModelSource::Openai);
    assert!(!listing.pull_supported);
    assert!(listing.pull_unsupported_reason.is_some());
    assert_eq!(listing.models[0].name, "test-model");
    assert_eq!(listing.models[0].size_bytes, None);
    let response = start_pull(&api, "llama3.2:3b").await;
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::NOT_IMPLEMENTED,
        "urn:genaimage:problem:pull-unsupported",
    )
    .await;
    assert!(problem.detail.contains("llama.cpp"));
}

#[tokio::test]
async fn unconfigured_llm_is_unavailable_not_empty() {
    let api = ApiHarness::start().await;
    let response = api.request(Method::GET, LIST).send().await.unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "urn:genaimage:problem:llm-not-configured",
    )
    .await;
    let response = start_pull(&api, "llama3.2:3b").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_pull_reports_exact_progress_can_be_reattached_and_succeeds() {
    let (server, api) = ollama(gated()).await;
    let pull = started(&api, "llama3.2:3b").await;
    assert_eq!(pull.state, ModelPullState::Running);

    let progress = wait_for(&api, &pull.id, |pull| pull.total_bytes.is_some()).await;
    assert_eq!(progress.total_bytes, Some(5_000_000_000_000_000));
    assert_eq!(progress.completed_bytes, Some(1_250_000_000_000_000));
    assert_eq!(progress.layer.as_deref(), Some("sha256:aaa"));
    let raw: Value = api.get_json(&format!("{PULLS}/{}", pull.id)).await;
    assert_eq!(raw["total_bytes"].to_string(), "5000000000000000");

    // A second request for the same model names the running pull.
    {
        let response = start_pull(&api, "llama3.2:3b").await;
        assert_eq!(
            response.headers()["location"],
            format!("{PULLS}/{}", pull.id).as_str()
        );
        let problem = ApiHarness::assert_typed_problem(
            response,
            StatusCode::CONFLICT,
            "urn:genaimage:problem:pull-in-progress",
        )
        .await;
        assert!(problem.detail.contains(&pull.id));
    }
    assert_eq!(server.pull_count(), 1);

    // A new client attaches mid-pull, as after a page reload.
    let events = api
        .request(Method::GET, &format!("{PULLS}/{}/events", pull.id))
        .send()
        .await
        .unwrap();
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    server.release_pulls();
    let frames = sse_frames(events).await;
    let names = frames
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names[0], "pull_progress");
    assert_eq!(*names.last().unwrap(), "pull_succeeded");
    assert_eq!(
        names
            .iter()
            .filter(|name| **name != "pull_progress")
            .count(),
        1
    );
    let sequences = frames
        .iter()
        .map(|(_, pull)| pull.sequence)
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));

    let done = finished(&api, &pull.id).await;
    assert_eq!(done.state, ModelPullState::Succeeded);
    assert_eq!(done.completed_bytes, done.total_bytes);
    assert!(done.finished_at.is_some() && done.error.is_none());
    let list: ModelPullListResponse = api.get_json(PULLS).await;
    assert_eq!(list.pulls[0].id, pull.id);

    // After completion the snapshot stays readable and a new pull is allowed again.
    let again = started(&api, "llama3.2:3b").await;
    assert_ne!(again.id, pull.id);
}

#[tokio::test]
async fn failed_and_truncated_pulls_are_failures_without_the_key() {
    for (steps, needle) in [
        (
            vec![FakePullStep::error(&format!("no access with {SECRET}"))],
            "no access",
        ),
        (
            vec![FakePullStep::progress("pulling manifest")],
            "without Ollama",
        ),
        (vec![FakePullStep::Raw("x".repeat(100_000))], "longer than"),
    ] {
        let (_server, api) = ollama(steps).await;
        let pull = started(&api, "llama3.2:3b").await;
        let done = finished(&api, &pull.id).await;
        assert_eq!(done.state, ModelPullState::Failed);
        let error = done.error.unwrap();
        assert!(error.contains(needle), "{error}");
        assert!(!error.contains(SECRET));
        let body = serde_json::to_string(&pull_state(&api, &pull.id).await).unwrap();
        assert!(!body.contains(SECRET));
    }
}

#[tokio::test]
async fn an_endpoint_error_at_start_is_reported_and_leaves_no_pull() {
    let server = FakeOllamaServer::start(FakeOllamaScript {
        pull_status: Some((StatusCode::NOT_FOUND, format!("missing ({SECRET})"))),
        ..Default::default()
    })
    .await;
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", &server.base_url()),
            ("GENAIMAGE_LLM_API_KEY", SECRET),
        ],
    )
    .await;
    let response = start_pull(&api, "nope").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.text().await.unwrap();
    assert!(body.contains("404") && !body.contains(SECRET), "{body}");
    let list: ModelPullListResponse = api.get_json(PULLS).await;
    assert!(list.pulls.is_empty());
}

#[tokio::test]
async fn invalid_names_and_bodies_are_rejected_without_a_network_call() {
    let (server, api) = ollama(gated()).await;
    for name in [
        "",
        "a b",
        "../x",
        "x\ny",
        "http://evil/x",
        "a/b/c/d",
        ":tag",
    ] {
        let response = start_pull(&api, name).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name:?}"
        );
    }
    let response = api
        .request(Method::POST, PULLS)
        .body(r#"{"name":"llama3.2:3b"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let response = api
        .request(Method::POST, PULLS)
        .json(&json!({"name": "llama3.2:3b", "stream": false}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let response = api
        .request(Method::POST, PULLS)
        .header("content-type", "application/json")
        .body(vec![b'a'; 8 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(server.pull_count(), 0);
    assert_eq!(server.version_count(), 0);
}

#[tokio::test]
async fn nothing_pulls_without_an_explicit_request() {
    let (server, api) = ollama(gated()).await;
    sleep(Duration::from_millis(300)).await; // startup background probes
    let _: LlmModelsResponse = api.get_json(LIST).await;
    let _: Value = api.get_json("/api/v1/doctor").await;
    let _: Value = api.get_json("/api/v1/health").await;
    let _: Value = api.get_json("/api/v1/settings").await;
    let saved = api
        .request(Method::PUT, "/api/v1/settings")
        .json(&json!({"comfyui.request_timeout_seconds": 7}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let _: LlmModelsResponse = api.get_json(LIST).await;
    let missing = api.request(Method::GET, PULLS).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::OK);
    assert_eq!(
        server.pull_count(),
        0,
        "a model was pulled without a user action"
    );
    assert!(server.tags_count() >= 2, "the fake was exercised");
}

#[tokio::test]
async fn too_many_concurrent_pulls_are_refused_and_cancel_reports_cancelled() {
    let (server, api) = ollama(gated()).await;
    let first = started(&api, "one:1b").await;
    let second = started(&api, "two:1b").await;
    let response = start_pull(&api, "three:1b").await;
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::TOO_MANY_REQUESTS,
        "urn:genaimage:problem:too-many-pulls",
    )
    .await;
    assert_eq!(server.pull_count(), 2);

    let cancelled: ModelPull = api
        .request(Method::DELETE, &format!("{PULLS}/{}", first.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled.state, ModelPullState::Cancelled);
    server.release_pulls();
    let done = finished(&api, &second.id).await;
    assert_eq!(done.state, ModelPullState::Succeeded);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        pull_state(&api, &first.id).await.state,
        ModelPullState::Cancelled,
        "a late event must not turn a cancelled pull into a success"
    );
    // The cancelled pull freed its slot.
    started(&api, "three:1b").await;
}

#[tokio::test]
async fn unknown_pull_ids_are_not_found_with_a_restart_hint() {
    let (_server, api) = ollama(gated()).await;
    for (method, suffix) in [
        (Method::GET, ""),
        (Method::DELETE, ""),
        (Method::GET, "/events"),
    ] {
        let response = api
            .request(method, &format!("{PULLS}/not-a-pull{suffix}"))
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_typed_problem(
            response,
            StatusCode::NOT_FOUND,
            "urn:genaimage:problem:pull-not-found",
        )
        .await;
        assert!(problem.detail.contains("restarts"));
    }
}

#[tokio::test]
async fn checkpoints_come_from_object_info_with_locations() {
    let api = ApiHarness::start().await;
    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["b.safetensors", "a.ckpt"], {}]}}}
    }));
    let response: CheckpointsResponse = api.get_json("/api/v1/models/checkpoints").await;
    assert_eq!(response.checkpoints, ["a.ckpt", "b.safetensors"]);
    let directories = response
        .locations
        .iter()
        .map(|location| location.directory.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        directories,
        [
            "/opt/comfyui/models/checkpoints",
            "<ComfyUI>/models/checkpoints"
        ]
    );
    assert!(response.locations[0].steps[1].contains("docker compose cp"));
    assert!(response.refresh_note.contains("restart"));

    *api.fake.object_info_override.lock().unwrap() = Some(json!({
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [[]]}}}
    }));
    let empty: CheckpointsResponse = api.get_json("/api/v1/models/checkpoints").await;
    assert!(
        empty.checkpoints.is_empty(),
        "an installed loader with no files is an empty list"
    );
}

#[tokio::test]
async fn checkpoints_without_comfyui_or_loader_are_an_error_not_an_empty_list() {
    let api = ApiHarness::start().await;
    api.fake
        .object_info_unavailable
        .store(true, Ordering::SeqCst);
    let response = api
        .request(Method::GET, "/api/v1/models/checkpoints")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "urn:genaimage:problem:comfyui-unavailable",
    )
    .await;
    api.fake
        .object_info_unavailable
        .store(false, Ordering::SeqCst);
    // The default fake lists the loader without its options.
    let response = api
        .request(Method::GET, "/api/v1/models/checkpoints")
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "urn:genaimage:problem:comfyui-unavailable",
    )
    .await;
    assert!(problem.detail.contains("CheckpointLoaderSimple"));
}

#[tokio::test]
async fn a_cut_event_stream_does_not_stop_the_pull_and_a_late_attach_sees_the_end() {
    let (server, api) = ollama(gated()).await;
    let pull = started(&api, "llama3.2:3b").await;
    wait_for(&api, &pull.id, |pull| pull.total_bytes.is_some()).await;
    let events = api
        .request(Method::GET, &format!("{PULLS}/{}/events", pull.id))
        .send()
        .await
        .unwrap();
    let mut body = events.bytes_stream();
    timeout(BOUND, body.next())
        .await
        .expect("first frame")
        .unwrap()
        .unwrap();
    drop(body);
    server.release_pulls();
    assert_eq!(
        finished(&api, &pull.id).await.state,
        ModelPullState::Succeeded
    );
    let late = api
        .request(Method::GET, &format!("{PULLS}/{}/events", pull.id))
        .send()
        .await
        .unwrap();
    let frames = sse_frames(late).await;
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, "pull_succeeded");
}

#[tokio::test]
async fn cross_origin_pull_requests_are_refused_without_a_download() {
    let (server, api) = ollama(gated()).await;
    let pull = started(&api, "llama3.2:3b").await;
    for (method, path) in [
        (Method::POST, PULLS.to_owned()),
        (Method::DELETE, format!("{PULLS}/{}", pull.id)),
    ] {
        let refused = api
            .request_with_origin(method, &path, Some("http://evil.example"))
            .json(&json!({ "name": "other:1b" }))
            .send()
            .await
            .unwrap();
        ApiHarness::assert_problem(refused, StatusCode::FORBIDDEN).await;
    }
    assert_eq!(server.pull_count(), 1);
    assert_eq!(
        pull_state(&api, &pull.id).await.state,
        ModelPullState::Running
    );
}
