mod common;

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use common::ApiHarness;
use genaimage_api::{HealthResponse, HealthStatus, ImageBackendHealthReason};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{Event, ImageBackend, RunRequest, RunStatus};
use reqwest::{header, Method, StatusCode};
use serde_json::json;

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
    let mut monitor = backend.monitor_events("test-client").await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        api.fake.wait_for_progress_listener(),
    )
    .await
    .unwrap();
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
