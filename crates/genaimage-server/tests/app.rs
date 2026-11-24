mod common;

use std::{
    collections::BTreeMap,
    future::{poll_fn, Future},
    sync::{atomic::Ordering, Arc},
    task::Poll,
    time::Duration,
};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    HealthResponse, HealthStatus, ImageBackendHealthReason, SettingSource, SettingsResponse,
    SettingsTestResponse,
};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{config::LlmConfig, Event, ImageBackend, LlmRoles, RunRequest, RunStatus};
use genaimage_llm::{OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions};
use genaimage_test_support::{FakeComfyServer, FakeComfyState, FakeLlmScript, FakeLlmServer};
use reqwest::{header, Method, StatusCode};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn run_request() -> RunRequest {
    RunRequest {
        workflow_revision: None,
        workflow: "contract-workflow".into(),
        preset: Some("fast".into()),
        parameters: BTreeMap::new(),
        version: Some("v0.1".into()),
    }
}

#[test]
fn health_response_decodes_without_hints_from_older_servers() {
    let health: HealthResponse = serde_json::from_value(json!({
        "self": "ok", "image_backend": "ok", "llm": "not_configured"
    }))
    .unwrap();
    assert!(health.hints.is_none());
}

#[tokio::test]
async fn health_reports_a_reachable_fake_and_emits_an_event() {
    let api = ApiHarness::start().await;
    let mut events = api.subscribe();
    let response = api
        .request(Method::GET, "/api/v1/health")
        .header("x-request-id", "health-check")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-request-id"], "health-check");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let health: HealthResponse = response.json().await.unwrap();
    assert_eq!(health.server, HealthStatus::Ok);
    assert_eq!(health.image_backend, HealthStatus::Ok);
    assert_eq!(health.llm, HealthStatus::NotConfigured);
    assert_eq!(health.image_backend_reason, None);
    let hints = health.hints.unwrap();
    assert!(hints.server.contains("GenAImage"));
    assert!(hints.image_backend.contains("comfyui.base_url"));
    assert!(hints.llm.contains("llm.base_url"));
    ApiHarness::await_event(&mut events, Duration::from_secs(2), |e| {
        matches!(e.event, Event::BackendHealthChanged { .. })
    })
    .await;
}

#[tokio::test]
async fn concurrent_role_lookups_single_flight_the_model_probe() {
    let llm = FakeLlmServer::start(
        FakeLlmScript::default()
            .with_models_response(json!({
                "object": "list",
                "data": [{ "id": "assistant", "supports_tools": true }]
            }))
            .with_models_response_delay(Duration::from_millis(100)),
    )
    .await;
    let config = LlmConfig {
        base_url: llm.base_url(),
        text_model: "assistant".into(),
        ..LlmConfig::default()
    };
    let provider = Arc::new(
        OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
            base_url: config.base_url.clone(),
            ..OpenAiCompatibleProviderOptions::default()
        })
        .unwrap(),
    );
    let roles = LlmRoles::new(
        &config,
        Some(provider as Arc<dyn genaimage_core::LlmProvider>),
    );
    let (first, second) = tokio::join!(roles.text(), roles.text());
    assert_eq!(first.unwrap().model(), "assistant");
    assert_eq!(second.unwrap().model(), "assistant");
    assert_eq!(llm.models_request_count(), 1);
}

#[tokio::test]
async fn health_reports_cached_llm_role_capabilities() {
    let llm = FakeLlmServer::start(FakeLlmScript::default().with_models_response(json!({
        "object": "list",
        "data": [{
            "id": "assistant",
            "supports_tools": true,
            "supports_vision": false
        }]
    })))
    .await;
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", llm.base_url().as_str()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "assistant"),
        ],
    )
    .await;
    let health = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let health: HealthResponse = api.get_json("/api/v1/health").await;
            if health.llm != HealthStatus::Unavailable
                || health.llm_reason.as_deref() != Some("The model capability probe is pending.")
            {
                return health;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LLM health probe");
    assert_eq!(health.llm, HealthStatus::Ok);
    assert_eq!(health.llm_text.unwrap().model.as_deref(), Some("assistant"));
    assert_eq!(
        health.llm_vision.unwrap().status,
        HealthStatus::NotConfigured
    );
}

#[tokio::test]
async fn health_reports_an_unreachable_llm_without_blocking() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let base_url = format!("http://{address}/v1");
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", &base_url),
            ("GENAIMAGE_LLM_TEXT_MODEL", "assistant"),
        ],
    )
    .await;
    let started = std::time::Instant::now();
    let health = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let health: HealthResponse = api.get_json("/api/v1/health").await;
            if health.llm_reason.as_deref() != Some("The model capability probe is pending.") {
                return health;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LLM failure probe");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(health.llm, HealthStatus::Unavailable);
    assert!(health.llm_reason.unwrap().contains("unavailable"));
}

#[tokio::test]
async fn health_reports_an_unavailable_backend_and_generates_request_ids() {
    let api = ApiHarness::start().await;
    api.fake.health_unavailable.store(true, Ordering::SeqCst);
    let mut ids = Vec::new();
    for _ in 0..2 {
        let response = api
            .request(Method::GET, "/api/v1/health")
            .send()
            .await
            .unwrap();
        ids.push(response.headers()["x-request-id"].clone());
        let health: HealthResponse = response.json().await.unwrap();
        assert_eq!(health.image_backend, HealthStatus::Unavailable);
        assert!(health
            .hints
            .as_ref()
            .unwrap()
            .image_backend
            .contains("Start ComfyUI"));
        assert_eq!(
            health.image_backend_reason,
            Some(ImageBackendHealthReason::ProbeFailed)
        );
    }
    assert!(!ids[0].is_empty());
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn health_times_out_when_backend_does_not_respond() {
    let api = ApiHarness::start().await;
    api.fake.health_hangs.store(true, Ordering::SeqCst);
    let health: HealthResponse =
        tokio::time::timeout(Duration::from_secs(5), api.get_json("/api/v1/health"))
            .await
            .expect("health deadline");
    assert_eq!(health.image_backend, HealthStatus::Unavailable);
    assert_eq!(
        health.image_backend_reason,
        Some(ImageBackendHealthReason::Timeout)
    );
}

#[tokio::test]
async fn api_routing_errors_are_problem_details() {
    let api = ApiHarness::start().await;
    for (method, path, status, title) in [
        (
            Method::GET,
            "/api/v1/missing",
            StatusCode::NOT_FOUND,
            "Not Found",
        ),
        (
            Method::POST,
            "/api/v1/health",
            StatusCode::METHOD_NOT_ALLOWED,
            "Method Not Allowed",
        ),
    ] {
        let response = api.request(method, path).send().await.unwrap();
        assert!(response.headers().contains_key("x-request-id"));
        if status == StatusCode::METHOD_NOT_ALLOWED {
            assert!(response.headers()[header::ALLOW]
                .to_str()
                .unwrap()
                .contains("GET"));
        }
        let problem = ApiHarness::assert_problem(response, status).await;
        assert_eq!(problem.title, title);
    }
}

#[tokio::test]
async fn independent_fixtures_can_script_submission_and_reconcile() {
    let (a, b) = tokio::join!(ApiHarness::start(), ApiHarness::start());
    assert_ne!(a.origin, b.origin);
    assert_ne!(a.workspace(), b.workspace());
    assert!(!a.fake.progress.same_channel(&b.fake.progress));
    install_workflow(&a);
    install_workflow(&b);
    a.fake.fail_submit_response.store(true, Ordering::SeqCst);
    b.fake.running.store(true, Ordering::SeqCst);
    let mut events = a.subscribe();
    let (unknown, running) = tokio::join!(a.submit_run(run_request()), b.submit_run(run_request()));
    let unknown = unknown.unwrap();
    let running = running.unwrap();
    assert_eq!(unknown.status, RunStatus::UnknownSubmission);
    assert_eq!(running.status, RunStatus::Queued);
    ApiHarness::await_event(
        &mut events,
        Duration::from_secs(2),
        |e| matches!(&e.event, Event::RunUnknownSubmission { run } if run.id == unknown.id),
    )
    .await;
    assert_eq!(
        a.core.reconcile(&unknown.id).await.unwrap().run.status,
        RunStatus::Queued
    );
    assert_eq!(a.fake.prompt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        b.core.cancel(&running.id).await.unwrap().status,
        RunStatus::Cancelling
    );
    assert_eq!(b.fake.interrupt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        b.core.reconcile(&running.id).await.unwrap().run.status,
        RunStatus::Cancelled
    );
    a.fake.complete.store(true, Ordering::SeqCst);
    let complete = a.core.reconcile(&unknown.id).await.unwrap();
    assert_eq!(complete.run.status, RunStatus::Completed);
    assert_eq!(complete.outputs.len(), 1);
}

#[tokio::test]
async fn missing_node_class_is_rejected_before_submission() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    api.fake
        .missing_classes
        .lock()
        .unwrap()
        .push("KSampler".into());
    assert!(api.submit_run(run_request()).await.is_err());
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.core.list_runs(10).unwrap().len(), 0);
}

#[tokio::test]
async fn fake_websocket_delivers_scripted_progress() {
    let api = ApiHarness::start().await;
    let backend = ComfyUiClient::new(&api.core.config().comfyui).unwrap();
    let first = api.fake.wait_for_progress_listener(Duration::from_secs(2));
    let second = api.fake.wait_for_progress_listener(Duration::from_secs(2));
    tokio::pin!(first, second);
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut monitor = backend.monitor_events("test-client").await.unwrap();
    let (first_result, second_result) = tokio::join!(first, second);
    first_result.unwrap();
    second_result.unwrap();
    api.fake
        .progress
        .send(
            json!({"type":"progress", "data":{"prompt_id":"p-1", "node":"3", "value":4, "max":20}}),
        )
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.event_type, "progress");
    assert_eq!(event.prompt_id.as_deref(), Some("p-1"));
    assert_eq!(event.value, Some(4));
}

#[tokio::test]
async fn requests_use_bound_host_and_same_origin_and_allow_override() {
    let api = ApiHarness::start().await;
    let get = api.request(Method::GET, "/api/v1/health").build().unwrap();
    assert_eq!(get.url().origin().ascii_serialization(), api.origin);
    assert!(!get.headers().contains_key(header::ORIGIN));
    let post = api.request(Method::POST, "/api/v1/health").build().unwrap();
    assert_eq!(post.headers()[header::ORIGIN], api.origin);
    let custom = api
        .request_with_origin(Method::POST, "/api/v1/health", Some("http://localhost:1"))
        .build()
        .unwrap();
    assert_eq!(custom.headers()[header::ORIGIN], "http://localhost:1");
    assert_eq!(api.core.config().server.port, get.url().port().unwrap());
    let bad_host = api
        .request(Method::GET, "/api/v1/health")
        .header(header::HOST, "evil.example:1234")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(bad_host, StatusCode::MISDIRECTED_REQUEST).await;
    let bad_origin = api
        .request_with_origin(Method::POST, "/api/v1/health", Some("http://localhost:1"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(bad_origin, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn fixture_uses_default_server_limits_and_explicit_backend_timeout() {
    let api = ApiHarness::start().await;
    assert_eq!(
        api.core.config().server.max_upload_bytes,
        genaimage_core::config::ServerConfig::default().max_upload_bytes
    );
    assert_eq!(api.core.config().comfyui.request_timeout_seconds, 5);
    for (path, size, expected) in [
        ("/api/v1/missing", 1024 * 1024, StatusCode::NOT_FOUND),
        (
            "/api/v1/missing",
            1024 * 1024 + 1,
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        ("/api/v1/uploads", 1024 * 1024 + 1, StatusCode::BAD_REQUEST),
    ] {
        let response = api
            .request(Method::POST, path)
            .body(vec![b'x'; size])
            .send()
            .await
            .unwrap();
        ApiHarness::assert_problem(response, expected).await;
    }
}

async fn stalled_request(api: &ApiHarness) -> tokio::net::TcpStream {
    let address = api.origin.strip_prefix("http://").unwrap();
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    client.write_all(format!(
        "POST /api/v1/health HTTP/1.1\r\nHost: {address}\r\nOrigin: {}\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n",
        api.origin,
    ).as_bytes()).await.unwrap();
    let mut interim = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !interim.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            client.read_exact(&mut byte).await.unwrap();
            interim.push(byte[0]);
        }
    })
    .await
    .expect("body was not polled");
    assert!(interim.starts_with(b"HTTP/1.1 100"));
    client
}

async fn assert_drained_response(mut client: tokio::net::TcpStream) {
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
        .await
        .expect("stalled request was not drained")
        .unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 503"),
        "{}",
        String::from_utf8_lossy(&response)
    );
}

#[tokio::test]
async fn shutdown_drains_stalled_request_before_removing_workspace() {
    let api = ApiHarness::start().await;
    let workspace = api.workspace().to_owned();
    let client = stalled_request(&api).await;
    api.shutdown().await;
    assert!(!workspace.exists());
    assert_drained_response(client).await;
}

#[tokio::test]
async fn drop_notifies_stalled_request() {
    let api = ApiHarness::start().await;
    let client = stalled_request(&api).await;
    drop(api);
    assert_drained_response(client).await;
}

#[tokio::test]
async fn dropping_fixture_removes_workspace_and_closes_listeners() {
    let api = ApiHarness::start().await;
    let workspace = api.workspace().to_owned();
    let address = api.origin.strip_prefix("http://").unwrap().to_owned();
    let backend_address = api
        .core
        .config()
        .comfyui
        .base_url
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    drop(api);
    assert!(!workspace.exists());
    for address in [address, backend_address] {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if tokio::net::TcpListener::bind(&address).await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("listener did not close");
    }
}

#[tokio::test]
async fn fake_queue_and_history_are_prompt_specific_and_overridable() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let run = api.submit_run(run_request()).await.unwrap();
    let backend = ComfyUiClient::new(&api.core.config().comfyui).unwrap();
    assert!(backend.queue().await.unwrap().contains(&run.prompt_id));
    backend.delete_queued("unrelated").await.unwrap();
    assert!(backend.queue().await.unwrap().contains(&run.prompt_id));
    api.fake.complete.store(true, Ordering::SeqCst);
    assert!(backend
        .history("unrelated")
        .await
        .unwrap()
        .outputs
        .is_empty());
    *api.fake.queue_override.lock().unwrap() =
        Some(json!({"queue_running": [[1, "other", {}, {}]], "queue_pending": []}));
    assert!(backend.queue().await.unwrap().is_running("other"));
    api.fake.history_overrides.lock().unwrap().insert(
        "other".into(),
        json!({"other": {"status": {"status_str": "success", "completed": true, "messages": []}, "outputs": {"9": {"images": [{"filename": "test.png"}]}}}}),
    );
    assert_eq!(
        backend.history("other").await.unwrap().outcome,
        genaimage_core::image_backend::HistoryOutcome::Completed
    );
}

#[tokio::test]
async fn timed_out_submission_is_reconciled_without_retry() {
    let api = ApiHarness::start_with_backend_timeout(1).await;
    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    let mut events = api.subscribe();
    let run = tokio::time::timeout(Duration::from_secs(5), api.submit_run(run_request()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::UnknownSubmission);
    ApiHarness::await_event(&mut events, Duration::from_secs(2), |e| matches!(&e.event, Event::RunUnknownSubmission { run: unknown } if unknown.id == run.id)).await;
    assert_eq!(
        api.core.reconcile(&run.id).await.unwrap().run.status,
        RunStatus::Queued
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
}

const SETTINGS: &str = "/api/v1/settings";
const TEST_SETTINGS: &str = "/api/v1/settings/test";

#[tokio::test]
async fn settings_report_sources_and_key_level_errors_without_writing() {
    let api = ApiHarness::start_with_config(5, &[("GENAIMAGE_LLM_TEXT_MODEL", "env-model")]).await;
    let path = api.workspace().join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    let settings: SettingsResponse = api.get_json(SETTINGS).await;
    assert_eq!(settings.config_file, path.to_str().unwrap());
    assert!(matches!(
        settings.settings["server.host"].source,
        SettingSource::Default
    ));
    assert!(
        matches!(&settings.settings["server.port"].source, SettingSource::File { file } if file == &settings.config_file)
    );
    let model = &settings.settings["llm.text_model"];
    assert_eq!(model.value, "env-model");
    assert!(model.read_only);
    assert!(
        matches!(&model.source, SettingSource::Env { variable } if variable == "GENAIMAGE_LLM_TEXT_MODEL")
    );
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({
            "comfyui.base_url": "http://user:must-not-echo@localhost",
            "paths.runs": "",
            "server.port": 0,
            "server.max_upload_bytes": "huge",
            "llm.text_model": "not-the-env-model",
            "unknown.flag": true
        }))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    let keys: std::collections::BTreeSet<_> = problem
        .errors
        .iter()
        .map(|error| error.key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "comfyui.base_url",
            "llm.text_model",
            "paths.runs",
            "server.max_upload_bytes",
            "server.port",
            "unknown.flag"
        ]
        .into()
    );
    assert!(!serde_json::to_string(&problem)
        .unwrap()
        .contains("must-not-echo"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    api.shutdown().await;
}

#[tokio::test]
async fn settings_expose_only_llm_api_key_presence() {
    let api = ApiHarness::start().await;
    let saved = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"llm.api_key": "never-return-this-secret"}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = saved.text().await.unwrap();
    assert!(!body.contains("never-return-this-secret"));
    let current: SettingsResponse = api.get_json(SETTINGS).await;
    assert!(current.api_key_set);
    assert!(!format!("{:?}", api.core).contains("never-return-this-secret"));
    assert!(!api.state_debug().contains("never-return-this-secret"));
    assert!(!current.settings.contains_key("llm.api_key"));
    assert!(current.restart_required.contains(&"llm.api_key".into()));
    api.shutdown().await;
}

#[tokio::test]
async fn settings_stage_endpoint_even_with_empty_database_and_apply_timing_live() {
    let api = ApiHarness::start().await;
    let alternate = FakeComfyServer::start(FakeComfyState::default()).await;
    api.fake.health_unavailable.store(true, Ordering::SeqCst);
    let port = api.core.config().server.port;
    let new_port = if port == 3001 { 3002 } else { 3001 };
    let path = api.workspace().join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    let patch = json!({
        "comfyui.base_url": format!("http://{}", alternate.address()),
        "server.port": new_port,
        "paths.state_database": "next.sqlite3",
        "comfyui.request_timeout_seconds": 1,
    });
    let probe: SettingsTestResponse = api.post_json(TEST_SETTINGS, &patch).await;
    assert_eq!(probe.comfyui, HealthStatus::Ok);
    assert_eq!(probe.llm, HealthStatus::NotConfigured);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let health: HealthResponse = api.get_json("/api/v1/health").await;
    assert_eq!(health.image_backend, HealthStatus::Unavailable);

    let rejected = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"comfyui.base_url": format!("http://{}", alternate.address())}))
        .send()
        .await
        .unwrap();
    let conflict = ApiHarness::assert_problem(rejected, StatusCode::CONFLICT).await;
    assert_eq!(conflict.errors[0].key, "comfyui.base_url");
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let mut events = api.subscribe();
    let saved = api
        .request(Method::PUT, SETTINGS)
        .json(&patch)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: SettingsResponse = saved.json().await.unwrap();
    assert_eq!(
        saved.restart_required,
        ["comfyui.base_url", "paths.state_database", "server.port"]
    );
    assert_eq!(
        saved.settings["server.port"].active_value,
        Some(json!(port))
    );
    assert_eq!(saved.settings["server.port"].value, new_port);
    assert_eq!(
        saved.settings["comfyui.base_url"].active_value,
        Some(json!(api.core.config().comfyui.base_url))
    );
    assert!(saved.settings["comfyui.request_timeout_seconds"]
        .active_value
        .is_none());
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(edited.starts_with("# API fixture configuration\n"));
    assert!(edited.contains(&format!("port = {new_port} # listener\n")));
    assert!(edited.contains("state_database = \"next.sqlite3\""));
    ApiHarness::await_event(&mut events, Duration::from_secs(2), |e| {
        matches!(&e.event, Event::SettingsChanged { settings } if settings["restart_required"] == json!(["comfyui.base_url", "paths.state_database", "server.port"]))
    }).await;
    let health: HealthResponse = api.get_json("/api/v1/health").await;
    assert_eq!(health.image_backend, HealthStatus::Unavailable);

    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    let run = tokio::time::timeout(Duration::from_secs(3), api.submit_run(run_request()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::UnknownSubmission);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn endpoint_and_separate_database_wait_for_restart_when_runs_exist() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let run = api.submit_run(run_request()).await.unwrap();
    let alternate_state = FakeComfyState::default();
    let alternate = FakeComfyServer::start(alternate_state.clone()).await;
    let url = format!("http://{}", alternate.address());
    let path = api.workspace().join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    use genaimage_core::persistence::RunRepository;
    let old_endpoint = api.core.config().comfyui.endpoint_url().unwrap();
    drop(
        RunRepository::open(&api.workspace().join("old.sqlite3"))
            .unwrap()
            .with_identity(api.workspace(), "ComfyUI", old_endpoint.as_str(), false)
            .unwrap(),
    );
    for patch in [
        json!({"comfyui.base_url": url}),
        json!({"comfyui.base_url": url, "paths.state_database": "old.sqlite3"}),
    ] {
        let staged = api
            .request(Method::PUT, SETTINGS)
            .json(&patch)
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_problem(staged, StatusCode::CONFLICT).await;
        assert_eq!(problem.errors[0].key, "comfyui.base_url");
        assert!(problem.recovery.contains("paths.state_database"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({
            "comfyui.base_url": url, "paths.state_database": "other.sqlite3"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved: SettingsResponse = response.json().await.unwrap();
    assert_eq!(
        saved.restart_required,
        ["comfyui.base_url", "paths.state_database"]
    );
    assert!(!api.workspace().join("other.sqlite3").exists());
    let compatible = api.workspace().join("compatible.sqlite3");
    drop(
        RunRepository::open(&compatible)
            .unwrap()
            .with_identity(api.workspace(), "ComfyUI", &format!("{url}/"), false)
            .unwrap(),
    );
    let db_original = std::fs::read(&compatible).unwrap();
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"paths.state_database": "compatible.sqlite3"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(std::fs::read(&compatible).unwrap(), db_original);
    api.fake.complete.store(true, Ordering::SeqCst);
    assert_eq!(
        api.core.reconcile(&run.id).await.unwrap().run.status,
        RunStatus::Completed
    );
    assert_eq!(alternate_state.prompt_calls.load(Ordering::SeqCst), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn settings_probe_refuses_non_loopback_and_times_out() {
    let api = ApiHarness::start().await;
    for (url, host) in [
        ("http://8.8.8.8", "8.8.8.8"),
        ("http://192.168.10.2", "192.168.10.2"),
    ] {
        let response = api
            .request(Method::POST, TEST_SETTINGS)
            .json(&json!({
                "comfyui.base_url": url, "comfyui.allowed_hosts": [host]
            }))
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(problem.errors[0].key, "comfyui.base_url");
    }
    let response = api
        .request_with_origin(Method::POST, TEST_SETTINGS, Some("https://example.test"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::FORBIDDEN).await;
    api.fake.health_hangs.store(true, Ordering::SeqCst);
    let probe: SettingsTestResponse = tokio::time::timeout(
        Duration::from_secs(5),
        api.post_json(TEST_SETTINGS, &json!({})),
    )
    .await
    .unwrap();
    assert_eq!(probe.comfyui, HealthStatus::Unavailable);
    assert_eq!(
        probe.comfyui_reason,
        Some(ImageBackendHealthReason::Timeout)
    );
    api.shutdown().await;
}

#[tokio::test]
async fn settings_reject_invalid_database_targets_without_touching_storage() {
    let api = ApiHarness::start().await;
    let root = api.workspace();
    std::fs::create_dir(root.join("directory")).unwrap();
    std::fs::write(root.join("file"), "not a directory").unwrap();
    let path = root.join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    let mut invalid = vec![
        "directory",
        "file/missing/state.sqlite3",
        "missing/",
        "genaimage.toml",
    ];
    #[cfg(unix)]
    let _socket = {
        let socket = std::os::unix::net::UnixListener::bind(root.join("special")).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("dangling")).unwrap();
        invalid.extend(["special", "dangling/state.sqlite3"]);
        socket
    };
    for target in invalid {
        let response = api
            .request(Method::PUT, SETTINGS)
            .json(&json!({"paths.state_database": target}))
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(problem.errors[0].key, "paths.state_database", "{target}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"paths.state_database": "new-directory/state.sqlite3"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!root.join("new-directory").exists());
    std::fs::write(root.join("empty.sqlite3"), []).unwrap();
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"paths.state_database": "empty.sqlite3"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        std::fs::metadata(root.join("empty.sqlite3")).unwrap().len(),
        0
    );
    api.shutdown().await;
}

#[tokio::test]
async fn settings_probe_llm_uses_models_route_without_persisting_or_following_redirects() {
    use axum::{response::Redirect, routing::get, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/models",
                    get(|| async { Json(json!({"object": "list", "data": []})) }),
                )
                .route(
                    "/v1/models",
                    get(|| async { Json(json!({"object": "list", "data": []})) }),
                )
                .route(
                    "/redirect/models",
                    get(|| async { Redirect::temporary("/v1/models") }),
                )
                .route("/stalled/models", get(std::future::pending::<StatusCode>)),
        )
        .await
        .unwrap();
    });
    let api = ApiHarness::start().await;
    let path = api.workspace().join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    for (suffix, status) in [
        ("", HealthStatus::Ok),
        ("/v1/", HealthStatus::Ok),
        ("/redirect", HealthStatus::Unavailable),
        ("/stalled", HealthStatus::Unavailable),
    ] {
        let response: SettingsTestResponse = tokio::time::timeout(
            Duration::from_secs(5),
            api.post_json(
                TEST_SETTINGS,
                &json!({"llm.base_url": format!("{root}{suffix}")}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.comfyui, HealthStatus::Ok);
        assert_eq!(response.llm, status, "{suffix}");
    }
    let response = api
        .request(Method::POST, TEST_SETTINGS)
        .json(
            &json!({"llm.base_url": "http://192.168.10.2", "llm.allowed_hosts": ["192.168.10.2"]}),
        )
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(problem.errors[0].key, "llm.base_url");
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let configured = ApiHarness::start_with_config(5, &[("GENAIMAGE_LLM_URL", &root)]).await;
    let response: SettingsTestResponse = configured.post_json(TEST_SETTINGS, &json!({})).await;
    assert_eq!(response.llm, HealthStatus::Ok);
    configured.fake.health_hangs.store(true, Ordering::SeqCst);
    let selected: SettingsTestResponse = tokio::time::timeout(
        Duration::from_secs(1),
        configured.post_json(&format!("{TEST_SETTINGS}?service=llm"), &json!({})),
    )
    .await
    .unwrap();
    assert_eq!(selected.llm, HealthStatus::Ok);
    assert_eq!(selected.comfyui, HealthStatus::NotConfigured);
    configured.shutdown().await;
    api.shutdown().await;
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn settings_reject_unwritable_database_targets_without_saving() {
    use std::{fs, os::unix::fs::PermissionsExt};

    if rustix::process::geteuid().is_root() {
        return;
    }
    let api = ApiHarness::start().await;
    let root = api.workspace();
    let config = root.join("genaimage.toml");
    let original = fs::read(&config).unwrap();
    let directory = root.join("locked");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("state.sqlite3"), []).unwrap();
    fs::write(root.join("readonly.sqlite3"), []).unwrap();
    fs::set_permissions(
        root.join("readonly.sqlite3"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
    let mut responses = Vec::new();
    for target in [
        "readonly.sqlite3",
        "locked/state.sqlite3",
        "locked/missing/state.sqlite3",
    ] {
        responses.push(
            api.request(Method::PUT, SETTINGS)
                .json(&json!({"paths.state_database": target}))
                .send()
                .await,
        );
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    for response in responses {
        let problem =
            ApiHarness::assert_problem(response.unwrap(), StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(problem.errors[0].key, "paths.state_database");
        assert!(problem.errors[0].reason.contains("writable"));
    }
    assert_eq!(fs::read(config).unwrap(), original);
    assert!(!directory.join("missing").exists());
    api.shutdown().await;
}

#[tokio::test]
async fn settings_etag_prevents_concurrent_lost_updates_and_preserves_legacy_put() {
    let api = ApiHarness::start().await;
    let response = api.request(Method::GET, SETTINGS).send().await.unwrap();
    let etag = response.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let first = api
        .request(Method::PUT, SETTINGS)
        .header(reqwest::header::IF_MATCH, &etag)
        .json(&json!({"llm.text_model":"first"}))
        .send();
    let second = api
        .request(Method::PUT, SETTINGS)
        .header(reqwest::header::IF_MATCH, &etag)
        .json(&json!({"llm.text_model":"second"}))
        .send();
    let (a, b) = tokio::join!(first, second);
    let (a, b) = (a.unwrap(), b.unwrap());
    let (saved, rejected) = if a.status() == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(saved.status(), StatusCode::OK);
    assert_ne!(saved.headers()[reqwest::header::ETAG], etag);
    let saved: SettingsResponse = saved.json().await.unwrap();
    let problem = ApiHarness::assert_problem(rejected, StatusCode::PRECONDITION_FAILED).await;
    assert!(problem.recovery.contains("Reload"));
    let current: SettingsResponse = api.get_json(SETTINGS).await;
    assert_eq!(
        current.settings["llm.text_model"].value,
        saved.settings["llm.text_model"].value
    );
    let legacy = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"llm.text_model":"legacy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);
    api.shutdown().await;
}

#[tokio::test]
async fn settings_probe_selects_service_and_uses_effective_env_url() {
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", "http://192.168.10.2"),
            ("GENAIMAGE_LLM_ALLOWED_HOSTS", "192.168.10.2"),
        ],
    )
    .await;
    let comfy: SettingsTestResponse = api
        .post_json(&format!("{TEST_SETTINGS}?service=comfyui"), &json!({}))
        .await;
    assert_eq!(comfy.comfyui, HealthStatus::Ok);
    assert_eq!(comfy.llm, HealthStatus::NotConfigured);
    let response = api
        .request(Method::POST, &format!("{TEST_SETTINGS}?service=llm"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(problem.errors[0].key, "llm.base_url");
    let invalid = api
        .request(Method::POST, &format!("{TEST_SETTINGS}?service=other"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(invalid, StatusCode::BAD_REQUEST).await;
    api.shutdown().await;

    let llm = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_COMFYUI_URL", "http://192.168.10.2"),
            ("GENAIMAGE_COMFYUI_ALLOWED_HOSTS", "192.168.10.2"),
            ("GENAIMAGE_LLM_URL", "http://127.0.0.1:1"),
        ],
    )
    .await;
    let result: SettingsTestResponse = llm
        .post_json(&format!("{TEST_SETTINGS}?service=llm"), &json!({}))
        .await;
    assert_eq!(result.comfyui, HealthStatus::NotConfigured);
    assert_eq!(result.llm, HealthStatus::Unavailable);
    llm.shutdown().await;
}

#[tokio::test]
async fn settings_probe_returns_model_capabilities() {
    let llm = FakeLlmServer::start(FakeLlmScript::default().with_models_response(json!({
        "object": "list",
        "data": [{
            "id": "assistant",
            "supports_tools": false,
            "supports_vision": true
        }]
    })))
    .await;
    let api = ApiHarness::start().await;
    let result: SettingsTestResponse = api
        .post_json(
            &format!("{TEST_SETTINGS}?service=llm"),
            &json!({"llm.base_url": llm.base_url()}),
        )
        .await;
    assert_eq!(result.llm, HealthStatus::Ok);
    assert_eq!(result.llm_models.len(), 1);
    assert_eq!(result.llm_models[0].id, "assistant");
    assert!(!result.llm_models[0].supports_tools);
    assert!(result.llm_models[0].supports_vision);
    api.shutdown().await;
}

#[tokio::test]
async fn settings_probe_checks_revision_against_its_snapshot() {
    let api = ApiHarness::start().await;
    let original = api.request(Method::GET, SETTINGS).send().await.unwrap();
    let old_etag = original.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let saved = api
        .request(Method::PUT, SETTINGS)
        .header(reqwest::header::IF_MATCH, &old_etag)
        .json(&json!({"llm.text_model": "changed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let new_etag = saved.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let before = api.fake.health_calls.load(Ordering::SeqCst);
    let stale = api
        .request(Method::POST, &format!("{TEST_SETTINGS}?service=comfyui"))
        .header(reqwest::header::IF_MATCH, &old_etag)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(stale, StatusCode::PRECONDITION_FAILED).await;
    assert!(problem.recovery.contains("Reload"));
    assert_eq!(api.fake.health_calls.load(Ordering::SeqCst), before);
    let stale_put = api
        .request(Method::PUT, SETTINGS)
        .header(reqwest::header::IF_MATCH, &old_etag)
        .json(&json!({"llm.text_model": "stale"}))
        .send()
        .await
        .unwrap();
    let put_problem = ApiHarness::assert_problem(stale_put, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(problem.detail, put_problem.detail);
    assert_eq!(problem.recovery, put_problem.recovery);
    let fresh = api
        .request(Method::POST, &format!("{TEST_SETTINGS}?service=comfyui"))
        .header(reqwest::header::IF_MATCH, &new_etag)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(fresh.status(), StatusCode::OK);
    let result: SettingsTestResponse = fresh.json().await.unwrap();
    assert_eq!(result.comfyui, HealthStatus::Ok);
    assert_eq!(api.fake.health_calls.load(Ordering::SeqCst), before + 1);
    let legacy: SettingsTestResponse = api.post_json(TEST_SETTINGS, &json!({})).await;
    assert_eq!(legacy.comfyui, HealthStatus::Ok);
    api.shutdown().await;
}
