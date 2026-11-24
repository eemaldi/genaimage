mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use futures_util::StreamExt;
use genaimage_api::{
    client::{Client, Error, EventsQuery, HeadEndpoint, OutputRequest, StreamEvent},
    Event, HealthStatus, OutputListQuery, ResyncEvent, RunEventsQuery, RunListQuery, RunStatus,
    SettingsPatch, SubmitRunRequest, ThumbnailQuery, WorkflowValidationRequest,
};
use reqwest::{header, StatusCode};
use serde_json::{json, Value};

const PNG: &[u8] = include_bytes!("fixtures/reference.png");
const WORKFLOW: &str = "contract-workflow";
const DEADLINE: Duration = Duration::from_secs(5);

fn submission() -> SubmitRunRequest {
    SubmitRunRequest {
        workflow: WORKFLOW.into(),
        preset: Some("fast".into()),
        parameters: [("seed".into(), json!(99))].into(),
        version: None,
    }
}

#[tokio::test]
async fn health_openapi_and_workflows() {
    let api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    assert_eq!(client.health().await.unwrap().server, HealthStatus::Ok);
    let openapi = client.openapi().await.unwrap();
    assert!(openapi["paths"]["/api/v1/events"]["get"].is_object());

    install_workflow(&api);
    let dir = &api.core.config().workflows;
    let manifest = dir.join("contract.genaimage.json");
    let mut body: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let id = "workflow / one? #";
    body["id"] = json!(id);
    body["thumbnail"] = json!("thumb.png");
    std::fs::write(&manifest, serde_json::to_vec(&body).unwrap()).unwrap();
    std::fs::write(dir.join("thumb.png"), PNG).unwrap();

    assert_eq!(client.workflows().await.unwrap().workflows[0].id, id);
    assert_eq!(client.workflow(id).await.unwrap().id, id);
    let validation = client
        .validate_workflow(
            id,
            &WorkflowValidationRequest {
                preset: Some("fast".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(validation.valid);
    assert_eq!(
        client
            .workflow_thumbnail(id)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        PNG
    );
    api.shutdown().await;
}

#[tokio::test]
async fn submits_lists_reconciles_and_cancels_runs_with_streaming_media() {
    let api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    install_workflow(&api);
    *api.fake.output_bytes.lock().unwrap() = Some(PNG.to_vec());

    let submitted = client.submit_run("complete", &submission()).await.unwrap();
    assert_eq!(submitted.status, RunStatus::Queued);
    assert_eq!(submitted.parameters["seed"], 99);
    assert_eq!(client.run(&submitted.id).await.unwrap().id, submitted.id);
    let runs = client
        .runs(&RunListQuery {
            workflow: Some(WORKFLOW.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(runs.runs[0].id, submitted.id);
    assert_eq!(
        client
            .run_events(&submitted.id, &RunEventsQuery { limit: Some(1) })
            .await
            .unwrap()
            .events
            .len(),
        1
    );
    api.fake.complete.store(true, Ordering::SeqCst);
    assert_eq!(
        client.reconcile_run(&submitted.id).await.unwrap().status,
        RunStatus::Completed
    );

    let manifest = client.run_manifest(&submitted.id, None).await.unwrap();
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/json");
    assert!(manifest.json::<Value>().await.unwrap().is_object());
    let outputs = client
        .outputs(&OutputListQuery {
            run: Some(submitted.id),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = &outputs.outputs[0].id;
    let original = client.output(id, &OutputRequest::default()).await.unwrap();
    let etag = original.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(original.bytes().await.unwrap(), PNG);
    let range = client
        .output(
            id,
            &OutputRequest {
                range: Some("bytes=1-3"),
                if_range: Some(&etag),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.bytes().await.unwrap(), &PNG[1..4]);
    let head = client
        .head(HeadEndpoint::Output(id, Some(&etag)))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(head.headers()[header::ETAG], etag);
    assert!(head.bytes().await.unwrap().is_empty());
    let thumb = client
        .output_thumbnail(id, &ThumbnailQuery { size: 128 }, None)
        .await
        .unwrap();
    assert_eq!(thumb.headers()[header::CONTENT_TYPE], "image/webp");
    assert!(!thumb.bytes().await.unwrap().is_empty());

    api.fake.complete.store(false, Ordering::SeqCst);
    api.fake.cancellation_history.store(false, Ordering::SeqCst);
    let second = client.submit_run("cancel", &submission()).await.unwrap();
    assert_eq!(
        client.cancel_run(&second.id).await.unwrap().status,
        RunStatus::Cancelling
    );
    let reconciled = client.reconcile_runs().await.unwrap();
    assert!(reconciled.runs.iter().any(|run| run.id == second.id));
    api.shutdown().await;
}

#[tokio::test]
async fn uploads_settings_and_typed_live_events() {
    let api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    let upload = client.upload("reference.png", PNG.to_vec()).await.unwrap();
    assert_eq!(upload.media_type, "image/png");
    assert_eq!(
        client.upload_metadata(&upload.upload_id).await.unwrap(),
        upload
    );

    let key = "comfyui.request_timeout_seconds";
    let patch = SettingsPatch([(key.into(), json!(3))].into());
    assert_eq!(client.settings().await.unwrap().settings[key].value, 5);
    assert_eq!(
        client.test_settings(&patch).await.unwrap().comfyui,
        HealthStatus::Ok
    );
    let mut stream = client.events(&EventsQuery::default()).await.unwrap();
    assert_eq!(
        client.save_settings(&patch).await.unwrap().settings[key].value,
        3
    );
    tokio::time::timeout(DEADLINE, async {
        loop {
            let item = stream.next().await.unwrap().unwrap();
            if let StreamEvent::Event(envelope) = item {
                if matches!(envelope.event, Event::SettingsChanged { .. }) {
                    break;
                }
            }
        }
    })
    .await
    .expect("settings_changed event");
    drop(stream);

    let mut stream = client
        .events(&EventsQuery {
            last_event_id: Some(u64::MAX),
            ..Default::default()
        })
        .await
        .unwrap();
    let next = tokio::time::timeout(DEADLINE, stream.next()).await.unwrap();
    assert!(matches!(
        next,
        Some(Ok(StreamEvent::Resync(ResyncEvent::CursorOutOfRange {
            requested: u64::MAX,
            ..
        })))
    ));
    assert!(tokio::time::timeout(DEADLINE, stream.next())
        .await
        .unwrap()
        .is_none());
    api.shutdown().await;
}

#[tokio::test]
async fn maps_problems_and_detects_a_stopped_server() {
    let mut api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    let error = client.workflow("missing").await.unwrap_err();
    let Error::Problem(problem) = error else {
        panic!("expected a problem, got {error}");
    };
    assert_eq!(problem.status, 404);
    assert_eq!(problem.title, "Not Found");
    assert!(problem.detail.contains("missing"));
    assert!(!problem.recovery.is_empty());
    let error = client
        .head(HeadEndpoint::Workflow("missing"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::HttpStatus {
            status: StatusCode::NOT_FOUND,
            ..
        }
    ));
    api.drain().await;
    let error = client.health().await.unwrap_err();
    assert!(matches!(error, Error::ServerNotRunning { .. }));
    assert!(error.to_string().contains("genaimage serve"));
}

#[tokio::test]
async fn uncertain_submission_returns_the_durable_run_for_reconciliation() {
    let api = ApiHarness::start_with_backend_timeout(1).await;
    let client = Client::with_base_url(&api.origin).unwrap();
    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    let error = client
        .submit_run("uncertain", &submission())
        .await
        .unwrap_err();
    let Error::UnknownSubmission(run) = error else {
        panic!("expected an unconfirmed submission, got {error}");
    };
    assert_eq!(run.status, RunStatus::UnknownSubmission);
    assert_eq!(
        client.reconcile_run(&run.id).await.unwrap().status,
        RunStatus::Queued
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn failed_submission_is_not_returned_as_a_successful_run() {
    let api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    install_workflow(&api);
    let runs = &api.core.config().runs;
    std::fs::create_dir_all(runs).unwrap();
    // Prevent run directory creation after the server reserves the idempotency key.
    std::fs::write(runs.join(WORKFLOW), b"not a directory").unwrap();
    let error = client
        .submit_run("failure", &submission())
        .await
        .unwrap_err();
    let Error::FailedSubmission(run) = error else {
        panic!("expected a failed run, got {error}");
    };
    assert_eq!(client.run(&run.id).await.unwrap().status, RunStatus::Failed);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn submission_does_not_follow_a_post_preserving_redirect() {
    use axum::{response::Redirect, routing::post, Router};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client =
        Client::with_base_url(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let router = Router::new()
        .route(
            "/api/v1/runs",
            post(|| async { Redirect::temporary("/replayed") }),
        )
        .route("/replayed", post(|| async { StatusCode::NO_CONTENT }));
    let (shutdown, done) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = done.await;
            })
            .await
            .unwrap();
    });
    let error = client
        .submit_run("do-not-replay", &submission())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::HttpStatus {
            status: StatusCode::TEMPORARY_REDIRECT,
            ..
        }
    ));
    shutdown.send(()).unwrap();
    server.await.unwrap();
}
