mod common;

use std::{
    collections::BTreeMap,
    future::{poll_fn, Future},
    sync::atomic::Ordering,
    task::Poll,
    time::Duration,
};

use common::ApiHarness;
use genaimage_api::{
    HealthResponse, HealthStatus, ImageBackendHealthReason, SettingSource, SettingsResponse,
    SettingsTestResponse,
};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{Event, ImageBackend, RunRequest, RunStatus};
use genaimage_test_support::{FakeComfyServer, FakeComfyState};
use reqwest::{header, Method, StatusCode};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn install_workflow(api: &ApiHarness) {
    let dir = &api.core.config().workflows;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("workflow.api.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
    )
    .unwrap();
    std::fs::write(
        dir.join("contract.genaimage.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"),
    )
    .unwrap();
}

fn run_request() -> RunRequest {
    RunRequest {
        workflow: "contract-workflow".into(),
        preset: Some("fast".into()),
        parameters: BTreeMap::new(),
        version: Some("v0.1".into()),
    }
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
    ApiHarness::await_event(&mut events, Duration::from_secs(2), |e| {
        matches!(e.event, Event::BackendHealthChanged { .. })
    })
    .await;
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
        ("/api/v1/uploads", 1024 * 1024 + 1, StatusCode::NOT_FOUND),
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
        json!({"other": {"status": {"status_str": "success", "completed": true, "messages": []}}}),
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
    let api = ApiHarness::start_with_config(5, &[("GENAIMAGE_LLM_MODEL", "env-model")]).await;
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
    let model = &settings.settings["llm.model"];
    assert_eq!(model.value, "env-model");
    assert!(model.read_only);
    assert!(
        matches!(&model.source, SettingSource::Env { variable } if variable == "GENAIMAGE_LLM_MODEL")
    );
    let response = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({
            "comfyui.base_url": "http://user:must-not-echo@localhost",
            "paths.runs": "",
            "server.port": 0,
            "server.max_upload_bytes": "huge",
            "llm.model": "not-the-env-model",
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
            "llm.model",
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
async fn settings_probe_then_save_and_hot_reload_an_empty_database() {
    let api = ApiHarness::start().await;
    let alternate = FakeComfyServer::start(FakeComfyState::default()).await;
    api.fake.health_unavailable.store(true, Ordering::SeqCst);
    let port = api.core.config().server.port;
    let new_port = if port == 3001 { 3002 } else { 3001 };
    let path = api.workspace().join("genaimage.toml");
    let original = std::fs::read(&path).unwrap();
    let patch = json!({"comfyui.base_url": format!("http://{}", alternate.address()), "server.port": new_port});
    let probe: SettingsTestResponse = api.post_json(TEST_SETTINGS, &patch).await;
    assert_eq!(probe.comfyui, HealthStatus::Ok);
    assert_eq!(probe.llm, HealthStatus::NotConfigured);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let health: HealthResponse = api.get_json("/api/v1/health").await;
    assert_eq!(health.image_backend, HealthStatus::Unavailable);

    let mut events = api.subscribe();
    let saved = api
        .request(Method::PUT, SETTINGS)
        .json(&patch)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: SettingsResponse = saved.json().await.unwrap();
    assert_eq!(saved.restart_required, ["server.port"]);
    assert_eq!(
        saved.settings["server.port"].active_value,
        Some(json!(port))
    );
    assert_eq!(saved.settings["server.port"].value, new_port);
    assert!(saved.settings["comfyui.base_url"].active_value.is_none());
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(edited.starts_with("# API fixture configuration\n"));
    assert!(edited.contains(&format!("port = {new_port} # listener\n")));
    assert!(edited.contains("state_database = \"state.sqlite3\""));
    ApiHarness::await_event(&mut events, Duration::from_secs(2), |e| {
        matches!(&e.event, Event::SettingsChanged { settings } if settings["restart_required"] == json!(["server.port"]))
    }).await;
    let health: HealthResponse = api.get_json("/api/v1/health").await;
    assert_eq!(health.image_backend, HealthStatus::Ok);

    install_workflow(&api);
    assert!(matches!(
        api.core.submit(run_request()).await,
        Err(genaimage_core::HarnessError::SettingsConflict(_))
    ));
    assert_eq!(
        api.submit_run(run_request()).await.unwrap().status,
        RunStatus::Queued
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
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
    let rejected = api
        .request(Method::PUT, SETTINGS)
        .json(&json!({"comfyui.base_url": url}))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(rejected, StatusCode::CONFLICT).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
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
