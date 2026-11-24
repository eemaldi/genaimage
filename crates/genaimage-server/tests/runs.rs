mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    ReconcileOutcome, ReconcileRunResponse, ReconcileRunsWithOutcomesResponse, RunEventsResponse,
    RunListResponse, RunResponse, RunStatus, WorkflowDetails, WorkflowListResponse,
    WorkflowValidationResponse,
};
use genaimage_core::Event;
use reqwest::{header, Method, Response, StatusCode};
use serde_json::{json, Value};

const RUNS: &str = "/api/v1/runs";
const DEADLINE: Duration = Duration::from_secs(5);

fn request() -> Value {
    json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 99}})
}

async fn submit(api: &ApiHarness, key: &str, body: &Value) -> Response {
    api.request(Method::POST, RUNS)
        .header("Idempotency-Key", key)
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn run(response: Response, expected: StatusCode) -> RunResponse {
    assert_eq!(response.status(), expected);
    response.json().await.unwrap()
}

const WORKFLOW: &str = "/api/v1/workflows/contract-workflow";

async fn validate(api: &ApiHarness, body: &Value) -> Response {
    api.request(Method::POST, &format!("{WORKFLOW}/validate"))
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn assert_workflow_changed(response: Response) {
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:workflow-changed",
    )
    .await;
    assert!(problem.recovery.contains("review it again"));
}

#[tokio::test]
async fn run_revision_uses_recorded_fingerprints_not_current_files() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let original: WorkflowDetails = api.get_json(WORKFLOW).await;
    let mut body = request();
    body["workflow_revision"] = json!(original.revision);
    let submitted = run(
        submit(&api, "recorded-revision", &body).await,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(submitted.workflow_revision, original.revision);

    let path = api.core.config().workflows.join("workflow.api.json");
    let mut graph: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    graph["9"]["inputs"]["filename_prefix"] = json!("changed");
    std::fs::write(&path, serde_json::to_vec(&graph).unwrap()).unwrap();
    let current: WorkflowDetails = api.get_json(WORKFLOW).await;
    assert_eq!(current.version, original.version);
    assert_ne!(current.revision, original.revision);
    let saved: RunResponse = api.get_json(&format!("{RUNS}/{}", submitted.id)).await;
    assert_eq!(saved.workflow_revision, original.revision);
    let listed: RunListResponse = api.get_json(RUNS).await;
    assert_eq!(listed.runs[0].workflow_revision, original.revision);

    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    db.execute(
        "UPDATE runs SET workflow_manifest_sha256 = NULL WHERE id = ?1",
        [&submitted.id],
    )
    .unwrap();
    drop(db);
    let legacy: Value = api.get_json(&format!("{RUNS}/{}", submitted.id)).await;
    assert!(legacy.get("workflow_revision").is_none());
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    db.execute(
        "UPDATE runs SET workflow_manifest_sha256 = 'invalid' WHERE id = ?1",
        [&submitted.id],
    )
    .unwrap();
    drop(db);
    let invalid: Value = api.get_json(&format!("{RUNS}/{}", submitted.id)).await;
    assert!(invalid.get("workflow_revision").is_none());
    api.shutdown().await;
}

#[tokio::test]
async fn graph_replacement_blocks_pinned_submit_and_validate_before_backend_io() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let reviewed: WorkflowDetails = api.get_json(WORKFLOW).await;
    assert!(reviewed.revision.is_some());
    let catalogue: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(catalogue.workflows[0].revision, reviewed.revision);

    let graph = api.core.config().workflows.join("workflow.api.json");
    let mut replacement: Value = serde_json::from_slice(&std::fs::read(&graph).unwrap()).unwrap();
    replacement["9"]["inputs"]["filename_prefix"] = json!("replacement");
    std::fs::write(&graph, serde_json::to_vec(&replacement).unwrap()).unwrap();

    let mut body = request();
    body["workflow_revision"] = json!(reviewed.revision);
    assert_workflow_changed(submit(&api, "graph-changed", &body).await).await;
    assert_workflow_changed(validate(&api, &json!({"workflow_revision": reviewed.revision})).await)
        .await;
    assert_eq!(api.fake.object_info_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api.get_json::<RunListResponse>(RUNS).await.runs.is_empty());

    let updated: WorkflowDetails = api.get_json(WORKFLOW).await;
    let catalogue: WorkflowListResponse = api.get_json("/api/v1/workflows").await;
    assert_eq!(catalogue.workflows[0].revision, updated.revision);
    assert_ne!(updated.revision, reviewed.revision);
    let response = validate(
        &api,
        &json!({"preset":"fast", "workflow_revision": updated.revision}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let validation: WorkflowValidationResponse = response.json().await.unwrap();
    assert!(validation.valid);
    assert_eq!(validation.revision, updated.revision);
    body["workflow_revision"] = json!(validation.revision);
    // The rejected attempt does not reserve the key.
    run(
        submit(&api, "graph-changed", &body).await,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn sidecar_change_blocks_stale_revision_but_unpinned_submit_uses_current_defaults() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["parameters"]["steps"]["default"] = json!(11);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let reviewed: WorkflowDetails = api.get_json(WORKFLOW).await;
    assert!(reviewed.revision.is_some());
    let mut body = json!({"workflow":"contract-workflow", "parameters":{"seed":99}, "workflow_revision":reviewed.revision});
    let original = run(submit(&api, "pinned", &body).await, StatusCode::ACCEPTED).await;
    assert_eq!(original.parameters["steps"], 11);
    let node_checks = api.fake.object_info_calls.load(Ordering::SeqCst);

    manifest["parameters"]["steps"]["default"] = json!(12);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert_workflow_changed(submit(&api, "stale", &body).await).await;
    assert_eq!(
        api.fake.object_info_calls.load(Ordering::SeqCst),
        node_checks
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);

    let replayed = run(submit(&api, "pinned", &body).await, StatusCode::OK).await;
    assert_eq!(replayed.id, original.id);
    assert_eq!(replayed.parameters["steps"], 11);
    let updated: WorkflowDetails = api.get_json(WORKFLOW).await;
    assert_ne!(updated.revision, reviewed.revision);
    body["workflow_revision"] = json!(updated.revision);
    let conflict =
        ApiHarness::assert_problem(submit(&api, "pinned", &body).await, StatusCode::CONFLICT).await;
    assert_eq!(conflict.problem_type, "about:blank");
    assert!(conflict.detail.contains("idempotency key"));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);

    body.as_object_mut().unwrap().remove("workflow_revision");
    let unpinned = run(submit(&api, "legacy", &body).await, StatusCode::ACCEPTED).await;
    assert_eq!(unpinned.parameters["steps"], 12);
    assert_eq!(unpinned.workflow_sha256, original.workflow_sha256);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[tokio::test]
async fn rerun_rejects_override_body_without_consuming_key_and_accepts_loader_free_workflow() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let source = run(
        submit(&api, "original", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let endpoint = format!("{RUNS}/{}/rerun", source.id);
    let response = api
        .request(Method::POST, &endpoint)
        .header("Idempotency-Key", "rerun")
        .json(&json!({"parameters": {"seed": 12345}}))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    assert!(problem.detail.contains("does not accept a request body"));
    let runs: RunListResponse = api.get_json(RUNS).await;
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    let response = api
        .request(Method::POST, &endpoint)
        .header("Idempotency-Key", "rerun")
        .send()
        .await
        .unwrap();
    let created = run(response, StatusCode::CREATED).await;
    assert_eq!(created.parameters, source.parameters);
    assert_ne!(created.id, source.id);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn submissions_and_reruns_reject_incomplete_workflow_discovery() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let source = run(
        submit(&api, "original", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let root = &api.core.config().workflows;
    let hidden = root.join("inaccessible");
    std::fs::create_dir(&hidden).unwrap();
    std::fs::copy(
        root.join("contract.genaimage.json"),
        hidden.join("duplicate.genaimage.json"),
    )
    .unwrap();
    std::fs::copy(
        root.join("workflow.api.json"),
        hidden.join("workflow.api.json"),
    )
    .unwrap();
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = Restore(hidden.clone());
    if std::fs::read_dir(&hidden).is_err() {
        let denied = submit(&api, "blocked-submit", &request()).await;
        let problem = ApiHarness::assert_problem(denied, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert!(problem
            .detail
            .contains("cannot resolve workflow ID 'contract-workflow' unambiguously"));
        assert!(problem.detail.contains("inaccessible"));
        assert!(problem.recovery.contains("permissions"));
        let endpoint = format!("{RUNS}/{}/rerun", source.id);
        let denied = api
            .request(Method::POST, &endpoint)
            .header("Idempotency-Key", "blocked-rerun")
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_problem(denied, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert!(problem
            .detail
            .contains("cannot resolve workflow ID 'contract-workflow' unambiguously"));
        assert!(problem.recovery.contains("permissions"));
        assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
        drop(restore);
        std::fs::remove_file(hidden.join("duplicate.genaimage.json")).unwrap();
        let response = api
            .request(Method::POST, &endpoint)
            .header("Idempotency-Key", "blocked-rerun")
            .send()
            .await
            .unwrap();
        let rerun = run(response, StatusCode::CREATED).await;
        assert_eq!(rerun.rerun_of.as_deref(), Some(source.id.as_str()));
        assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    } else {
        drop(restore);
    }
    api.shutdown().await;
}

#[tokio::test]
async fn rerun_refuses_a_replaced_comfyui_side_file_without_a_binding() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let graph_path = api.core.config().workflows.join("workflow.api.json");
    let input = api.workspace().join("comfyui-input");
    std::fs::create_dir(&input).unwrap();
    let image = input.join("literal.png");
    std::fs::write(&image, b"original image").unwrap();
    let mut graph: Value = serde_json::from_slice(&std::fs::read(&graph_path).unwrap()).unwrap();
    graph["12"] = json!({"class_type": "LoadImage", "inputs": {"image": "literal.png"}});
    std::fs::write(&graph_path, serde_json::to_vec(&graph).unwrap()).unwrap();
    let source = run(
        submit(&api, "original", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    // Replace only the backend-side file; leave the submitted workflow and manifest unchanged.
    std::fs::write(image, b"replacement image").unwrap();
    let response = api
        .request(Method::POST, &format!("{RUNS}/{}/rerun", source.id))
        .header("Idempotency-Key", "rerun")
        .send()
        .await
        .unwrap();
    let problem = rerun_mismatch(response).await;
    assert!(problem.detail.contains("ComfyUI-side file"));
    assert!(problem.recovery.contains("image upload parameter"));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn rerun_refuses_migrated_completed_run_even_with_indexed_manifest() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    api.fake.complete.store(true, Ordering::SeqCst);
    let source = run(
        submit(&api, "original", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let endpoint = format!("{RUNS}/{}", source.id);
    let completed: RunResponse = api
        .post_json(&format!("{endpoint}/reconcile"), &Value::Null)
        .await;
    assert_eq!(completed.status, RunStatus::Completed);
    let path = &api.core.config().state_database;
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "ALTER TABLE runs DROP COLUMN rerun_of;
        ALTER TABLE runs DROP COLUMN workflow_manifest_sha256;
        ALTER TABLE runs DROP COLUMN preflight_sha256; ALTER TABLE runs DROP COLUMN resolved_workflow_sha256;
        PRAGMA user_version = 7;",
    )
    .unwrap();
    drop(db);
    drop(genaimage_core::persistence::RunRepository::open(path).unwrap());
    assert!(
        api.core
            .open_run_manifest(&source.id)
            .await
            .unwrap()
            .integrity_verified
    );
    let response = api
        .request(Method::POST, &format!("{endpoint}/rerun"))
        .header("Idempotency-Key", "rerun")
        .send()
        .await
        .unwrap();
    let problem = rerun_mismatch(response).await;
    assert!(problem
        .detail
        .contains("submission-time workflow manifest fingerprint"));
    assert!(problem
        .recovery
        .contains("review the saved parameters and submit a new run"));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

async fn rerun_mismatch(response: Response) -> genaimage_api::ProblemDetails {
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let problem: genaimage_api::ProblemDetails = response.json().await.unwrap();
    assert_eq!(problem.problem_type, "urn:genaimage:problem:rerun-mismatch");
    problem
}

#[tokio::test]
async fn submits_once_concurrently_reports_progress_and_replays_current_run() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let mut subscription = api.subscribe();
    let first = request();
    // Same typed body, with a default field supplied explicitly.
    let second = json!({"version": null, "parameters": {"seed": 99}, "preset": "fast", "workflow": "contract-workflow"});
    let (a, b) = tokio::time::timeout(DEADLINE, async {
        tokio::join!(submit(&api, "same", &first), submit(&api, "same", &second))
    })
    .await
    .unwrap();
    let (created, replay) = if a.status() == StatusCode::ACCEPTED {
        (a, b)
    } else {
        (b, a)
    };
    let location = created.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let created = run(created, StatusCode::ACCEPTED).await;
    assert_eq!(location, format!("{RUNS}/{}", created.id));
    assert_eq!(created.status, RunStatus::Queued);
    assert_eq!(created.parameters["seed"], 99);
    assert_eq!(run(replay, StatusCode::OK).await.id, created.id);
    ApiHarness::assert_problem(
        submit(&api, "same", &json!({"workflow": "elsewhere"})).await,
        StatusCode::CONFLICT,
    )
    .await;
    api.fake.wait_for_progress_listener(DEADLINE).await.unwrap();
    let alternate_state = genaimage_test_support::FakeComfyState::default();
    let alternate = genaimage_test_support::FakeComfyServer::start(alternate_state.clone()).await;
    let update = api
        .request(Method::PUT, "/api/v1/settings")
        .json(&json!({
            "comfyui.request_timeout_seconds": 2,
            "comfyui.base_url": format!("http://{}", alternate.address()),
            "paths.state_database": "next.sqlite3",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    let update: genaimage_api::SettingsResponse = update.json().await.unwrap();
    assert_eq!(
        update.restart_required,
        ["comfyui.base_url", "paths.state_database"]
    );

    api.fake
        .progress
        .send(json!({"type":"progress", "data":{
            "prompt_id":created.prompt_id, "node":"3", "value":4, "max":20
        }}))
        .unwrap();
    ApiHarness::await_event(&mut subscription, DEADLINE, |e| {
        matches!(&e.event, Event::RunProgress { run, backend_event: Some(progress) }
            if run.id == created.id && progress.value == Some(4))
    })
    .await;
    api.fake.complete.store(true, Ordering::SeqCst);
    ApiHarness::await_event(
        &mut subscription,
        DEADLINE,
        |e| matches!(&e.event, Event::RunCompleted { run } if run.id == created.id),
    )
    .await;
    let current: RunResponse = api.get_json(&location).await;
    assert_eq!(current.status, RunStatus::Completed);
    let replay = run(submit(&api, "same", &first).await, StatusCode::OK).await;
    assert_eq!(replay.status, RunStatus::Completed);
    assert_eq!(replay.id, current.id);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    let history: RunEventsResponse = api.get_json(&format!("{location}/events?limit=2")).await;
    assert_eq!(history.events.len(), 2);
    assert!(history.events[0].id < history.events[1].id);
    assert_eq!(history.events[1].event_type, "run_completed");
    assert_eq!(alternate_state.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(!api.workspace().join("next.sqlite3").exists());
    api.shutdown().await;
}

#[tokio::test]
async fn ambiguous_timeout_is_persisted_and_only_reconciled_never_resubmitted() {
    let api = ApiHarness::start_with_backend_timeout(1).await;
    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    let submitted = run(
        submit(&api, "uncertain", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(submitted.status, RunStatus::UnknownSubmission);
    let replay = run(submit(&api, "uncertain", &request()).await, StatusCode::OK).await;
    assert_eq!(replay.id, submitted.id);
    assert_eq!(replay.status, RunStatus::UnknownSubmission);
    let path = format!("{RUNS}/{}", submitted.id);
    let history: RunEventsResponse = api.get_json(&format!("{path}/events")).await;
    assert!(history
        .events
        .iter()
        .any(|event| event.event_type == "submission_unknown"));
    let reconcile: ReconcileRunResponse = api
        .post_json(&format!("{path}/reconcile"), &Value::Null)
        .await;
    assert_eq!(reconcile.run.status, RunStatus::Queued);
    assert_eq!(reconcile.outcome, ReconcileOutcome::Active);
    api.fake.complete.store(true, Ordering::SeqCst);
    let bulk: ReconcileRunsWithOutcomesResponse = api
        .post_json(&format!("{RUNS}/reconcile"), &Value::Null)
        .await;
    assert_eq!(bulk.runs.len(), 1);
    assert_eq!(bulk.runs[0].run.id, submitted.id);
    assert_eq!(bulk.runs[0].run.status, RunStatus::Completed);
    assert_eq!(bulk.runs[0].outcome, ReconcileOutcome::Terminal);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn late_queue_admission_is_ambiguous_until_adopted_and_monitored() {
    use futures_util::StreamExt;
    use genaimage_api::client::{Client, EventsQuery, StreamEvent};
    let api = ApiHarness::start_with_backend_timeout(1).await;
    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    api.fake
        .defer_prompt_admission
        .store(true, Ordering::SeqCst);
    let submitted = run(submit(&api, "late", &request()).await, StatusCode::ACCEPTED).await;
    let client = Client::with_base_url(&api.origin).unwrap();
    let mut stream = client.events(&EventsQuery::default()).await.unwrap();
    let settling = client
        .reconcile_run_with_outcome(&submitted.id)
        .await
        .unwrap();
    assert_eq!(settling.outcome, ReconcileOutcome::StillAmbiguous);
    assert!(settling.settles_at.is_some());
    // Simulate ComfyUI finishing admission only after both absence reads.
    *api.fake.prompt_id.lock().unwrap() = Some(submitted.prompt_id.clone());
    let bulk = client.reconcile_runs_with_outcomes().await.unwrap();
    assert_eq!(bulk.runs[0].outcome, ReconcileOutcome::Active);
    assert_eq!(
        client
            .reconcile_run_with_outcome(&submitted.id)
            .await
            .unwrap()
            .outcome,
        ReconcileOutcome::Active
    );
    api.fake.wait_for_progress_listener(DEADLINE).await.unwrap();
    assert_eq!(api.fake.progress.receiver_count(), 1);
    api.fake.complete.store(true, Ordering::SeqCst);
    tokio::time::timeout(DEADLINE, async {
        while let Some(item) = stream.next().await {
            if let StreamEvent::Event(envelope) = item.unwrap() {
                if matches!(envelope.event, genaimage_api::Event::RunCompleted { run } if run.id == submitted.id) {
                    return;
                }
            }
        }
        panic!("the event stream closed before completion");
    }).await.expect("completion SSE event");
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    drop(stream);
    api.shutdown().await;
}

#[tokio::test]
async fn history_error_after_adoption_does_not_prevent_completion_sse() {
    use futures_util::StreamExt;
    use genaimage_api::client::{Client, EventsQuery, StreamEvent};
    for bulk in [false, true] {
        let api = ApiHarness::start().await;
        install_workflow(&api);
        api.fake.fail_submit_response.store(true, Ordering::SeqCst);
        let submitted = run(
            submit(&api, "recover", &request()).await,
            StatusCode::ACCEPTED,
        )
        .await;
        assert_eq!(submitted.status, RunStatus::UnknownSubmission);
        let client = Client::with_base_url(&api.origin).unwrap();
        let mut stream = client.events(&EventsQuery::default()).await.unwrap();
        api.fake.fail_next_history.store(true, Ordering::SeqCst);
        let path = if bulk {
            format!("{RUNS}/reconcile")
        } else {
            format!("{RUNS}/{}/reconcile", submitted.id)
        };
        let response = api.request(Method::POST, &path).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "bulk={bulk}");
        assert_eq!(
            client.run(&submitted.id).await.unwrap().status,
            RunStatus::Queued
        );
        api.fake.wait_for_progress_listener(DEADLINE).await.unwrap();
        api.fake.complete.store(true, Ordering::SeqCst);
        tokio::time::timeout(DEADLINE, async {
            while let Some(item) = stream.next().await {
                if let StreamEvent::Event(envelope) = item.unwrap() {
                    if matches!(envelope.event, genaimage_api::Event::RunCompleted { run } if run.id == submitted.id) {
                        return;
                    }
                }
            }
            panic!("stream closed before completion, bulk={bulk}");
        }).await.expect("completion SSE event after a failed reconcile");
        drop(stream);
        api.shutdown().await;
    }
}

#[tokio::test]
async fn distinguishes_missing_history_from_incomplete_history_without_resubmitting() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    api.fake.fail_submit_response.store(true, Ordering::SeqCst);
    let submitted = run(
        submit(&api, "unknown", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let path = format!("{RUNS}/{}", submitted.id);
    *api.fake.queue_override.lock().unwrap() =
        Some(json!({"queue_pending": [], "queue_running": []}));
    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    db.execute("UPDATE run_events SET created_at = '2000-01-01T00:00:00Z' WHERE run_id = ?1 AND event_type = 'submission_attempted'", [&submitted.id]).unwrap();
    for (history, expected) in [
        (
            json!({submitted.prompt_id.clone(): {"status": {"completed": false}}}),
            ReconcileOutcome::StillAmbiguous,
        ),
        (json!(null), ReconcileOutcome::StillAmbiguous),
        (
            json!({"error": "backend busy"}),
            ReconcileOutcome::StillAmbiguous,
        ),
        (json!({}), ReconcileOutcome::NotFound),
    ] {
        api.fake
            .history_overrides
            .lock()
            .unwrap()
            .insert(submitted.prompt_id.clone(), history);
        let result: ReconcileRunResponse = api
            .post_json(&format!("{path}/reconcile"), &Value::Null)
            .await;
        assert_eq!(result.run.status, RunStatus::UnknownSubmission);
        assert_eq!(result.run.updated_at, submitted.updated_at);
        assert_eq!(result.outcome, expected);
    }
    let bulk: ReconcileRunsWithOutcomesResponse = api
        .post_json(&format!("{RUNS}/reconcile"), &Value::Null)
        .await;
    assert_eq!(bulk.runs[0].run.id, submitted.id);
    assert_eq!(bulk.runs[0].outcome, ReconcileOutcome::NotFound);
    let events: RunEventsResponse = api.get_json(&format!("{path}/events")).await;
    assert_eq!(
        events.events.last().unwrap().payload,
        json!({"outcome": "not_found"})
    );
    *api.fake.queue_override.lock().unwrap() = Some(json!({"queue_pending": []}));
    let response = api
        .request(Method::POST, &format!("{path}/reconcile"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn cancels_pending_and_executing_prompts_with_the_appropriate_backend_action() {
    for running in [false, true] {
        let api = ApiHarness::start().await;
        install_workflow(&api);
        api.fake.running.store(running, Ordering::SeqCst);
        api.fake.cancellation_history.store(false, Ordering::SeqCst);
        let submitted = run(
            submit(&api, "cancel", &request()).await,
            StatusCode::ACCEPTED,
        )
        .await;
        let path = format!("{RUNS}/{}", submitted.id);
        let response = api
            .request(Method::POST, &format!("{path}/cancel"))
            .send()
            .await
            .unwrap();
        let result = run(response, StatusCode::OK).await;
        assert_eq!(result.status, RunStatus::Cancelling);
        assert_eq!(
            api.fake.interrupt_calls.load(Ordering::SeqCst),
            usize::from(running)
        );
        assert!(api.fake.cancelled.load(Ordering::SeqCst));
        let events: RunEventsResponse = api.get_json(&format!("{path}/events")).await;
        assert!(events.events.iter().any(|event| event.event_type
            == if running {
                "interrupt_requested"
            } else {
                "delete_requested"
            }));
        if running {
            api.fake.cancellation_history.store(true, Ordering::SeqCst);
            let response = api
                .request(Method::POST, &format!("{path}/reconcile"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                run(response, StatusCode::OK).await.status,
                RunStatus::Cancelled
            );
        }
        api.shutdown().await;
    }
}

#[tokio::test]
async fn lists_with_stable_pages_and_filters_and_reports_problem_details() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let first = run(
        submit(&api, "page-a", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let second = run(
        submit(&api, "page-b", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let filters = "workflow=contract-workflow&status=queued&limit=1";
    let page: RunListResponse = api.get_json(&format!("{RUNS}?{filters}")).await;
    assert_eq!(page.runs[0].id, second.id);
    let cursor = page.next_cursor.unwrap();
    let third = run(
        submit(&api, "page-c", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let page: RunListResponse = api
        .get_json(&format!("{RUNS}?{filters}&cursor={cursor}"))
        .await;
    assert_eq!(page.runs[0].id, first.id);
    assert!(page.next_cursor.is_none());
    let page: RunListResponse = api
        .get_json(&format!(
            "{RUNS}?created_from={}&created_to={}",
            second
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            third
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
        ))
        .await;
    assert_eq!(
        page.runs.into_iter().map(|run| run.id).collect::<Vec<_>>(),
        [third.id, second.id]
    );
    for query in [
        "status=failed",
        "workflow=other",
        "created_from=2099-01-01T00:00:00Z",
    ] {
        let empty: RunListResponse = api.get_json(&format!("{RUNS}?{query}")).await;
        assert!(empty.runs.is_empty());
    }
    let missing_key = api
        .request(Method::POST, RUNS)
        .json(&request())
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(missing_key, StatusCode::BAD_REQUEST).await;
    for (path, status) in [
        (format!("{RUNS}?cursor=garbage"), StatusCode::BAD_REQUEST),
        (format!("{RUNS}?limit=201"), StatusCode::BAD_REQUEST),
        (format!("{RUNS}?status=nope"), StatusCode::BAD_REQUEST),
        (format!("{RUNS}/missing/events"), StatusCode::NOT_FOUND),
    ] {
        ApiHarness::assert_problem(
            api.request(Method::GET, &path).send().await.unwrap(),
            status,
        )
        .await;
    }
    api.shutdown().await;
}

#[tokio::test]
async fn hot_reload_keeps_disconnected_submission_on_its_snapshot_and_drains_it() {
    use tokio::{
        io::AsyncWriteExt,
        net::TcpStream,
        time::{timeout, Instant},
    };

    let drain = Duration::from_millis(150);
    let mut api = ApiHarness::start_with_deadlines(30, drain).await;
    install_workflow(&api);
    api.fake.hang_submit_response.store(true, Ordering::SeqCst);
    let address = api.origin.strip_prefix("http://").unwrap();
    let mut conn = TcpStream::connect(address).await.unwrap();
    let body = request().to_string();
    conn.write_all(format!(
        "POST {RUNS} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nIdempotency-Key: drain\r\nContent-Length: {}\r\n\r\n{body}", body.len(),
    ).as_bytes()).await.unwrap();
    timeout(DEADLINE, async {
        while api.fake.prompt_id.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ComfyUI received prompt");
    drop(conn);
    let original = api.core.list_runs(1).unwrap().remove(0);
    let update = api
        .request(Method::PUT, "/api/v1/settings")
        .json(&json!({"comfyui.request_timeout_seconds": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    let later = timeout(DEADLINE, submit(&api, "short-timeout", &request()))
        .await
        .unwrap();
    assert_eq!(
        run(later, StatusCode::ACCEPTED).await.status,
        RunStatus::UnknownSubmission
    );
    assert_eq!(
        api.core.get_run(&original.id).unwrap().status,
        genaimage_core::RunStatus::Submitting
    );
    let start = Instant::now();
    api.drain().await;
    assert!(
        start.elapsed() >= drain,
        "the drain must allow time for acknowledgement"
    );
    let replay = api
        .core
        .submit_keyed("drain".into(), serde_json::from_value(request()).unwrap())
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.run.status,
        genaimage_core::RunStatus::UnknownSubmission
    );
    assert!(replay
        .run
        .error
        .unwrap()
        .contains("Shutdown deadline expired"));
    assert_eq!(replay.run.id, original.id);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn targeted_interrupt_does_not_interrupt_a_replacement_and_refuses_older_comfyui() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    api.fake.running.store(true, Ordering::SeqCst);
    let submitted = run(
        submit(&api, "cancel-target", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let path = format!("{RUNS}/{}/cancel", submitted.id);
    for unsupported in [Some("0.3.56".into()), None] {
        *api.fake.comfyui_version.lock().unwrap() = unsupported;
        let response = api.request(Method::POST, &path).send().await.unwrap();
        let problem = ApiHarness::assert_problem(response, StatusCode::BAD_GATEWAY).await;
        assert!(problem.recovery.contains("Upgrade"));
        assert_eq!(api.fake.interrupt_calls.load(Ordering::SeqCst), 0);
    }
    *api.fake.comfyui_version.lock().unwrap() = Some("0.3.57".into());
    *api.fake.interrupt_queue_head.lock().unwrap() = Some("different-prompt".into());
    let response = api.request(Method::POST, &path).send().await.unwrap();
    assert_eq!(
        run(response, StatusCode::OK).await.status,
        RunStatus::Cancelling
    );
    assert_eq!(
        api.fake.interrupt_target.lock().unwrap().as_deref(),
        Some(submitted.prompt_id.as_str())
    );
    assert_eq!(api.fake.interrupt_calls.load(Ordering::SeqCst), 1);
    assert!(api.fake.interrupted_prompt_id.lock().unwrap().is_none());
    api.shutdown().await;
}

#[tokio::test]
async fn accepted_partial_prompt_persists_warnings_and_monitors_to_completion() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let warnings = json!({"9": {"errors": [{"message": "one output failed validation"}]}});
    *api.fake.node_errors.lock().unwrap() = warnings.clone();
    let mut subscription = api.subscribe();
    let submitted = run(
        submit(&api, "partial", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(submitted.status, RunStatus::Queued);
    assert_eq!(submitted.node_warnings.as_ref(), Some(&warnings));
    let path = format!("{RUNS}/{}", submitted.id);
    let persisted: RunEventsResponse = api.get_json(&format!("{path}/events")).await;
    assert!(persisted.events.iter().any(
        |event| event.event_type == "prompt_queued" && event.payload["node_errors"] == warnings
    ));
    api.fake.complete.store(true, Ordering::SeqCst);
    ApiHarness::await_event(
        &mut subscription,
        DEADLINE,
        |e| matches!(&e.event, Event::RunCompleted { run } if run.id == submitted.id),
    )
    .await;
    let persisted: RunResponse = api.get_json(&path).await;
    assert_eq!(persisted.status, RunStatus::Completed);
    assert_eq!(persisted.node_warnings, Some(warnings));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn shutdown_rolls_back_unsent_cancellation_and_records_uncertain_send() {
    for attempted in [false, true] {
        let mut api = ApiHarness::start_with_deadlines(30, Duration::from_millis(150)).await;
        install_workflow(&api);
        let submitted = run(
            submit(&api, "cancel-during-shutdown", &request()).await,
            StatusCode::ACCEPTED,
        )
        .await;
        api.fake.running.store(true, Ordering::SeqCst);
        api.fake.cancellation_history.store(false, Ordering::SeqCst);
        api.fake.health_hangs.store(!attempted, Ordering::SeqCst);
        api.fake
            .hang_interrupt_response
            .store(attempted, Ordering::SeqCst);
        let request = api.request(Method::POST, &format!("{RUNS}/{}/cancel", submitted.id));
        let pending = tokio::spawn(request.send());
        tokio::time::timeout(DEADLINE, async {
            let calls = if attempted {
                &api.fake.interrupt_calls
            } else {
                &api.fake.health_calls
            };
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancellation reached the scripted gate");
        assert_eq!(
            api.core.get_run(&submitted.id).unwrap().status,
            genaimage_core::RunStatus::Cancelling
        );
        pending.abort();
        let _ = pending.await;
        api.drain().await;
        let current = api.core.get_run(&submitted.id).unwrap();
        let events = api.core.run_events(&submitted.id, 20).unwrap();
        if attempted {
            assert_eq!(current.status, genaimage_core::RunStatus::Cancelling);
            assert!(events
                .iter()
                .any(|event| event.event_type == "cancellation_unknown"));
            api.fake.cancellation_history.store(true, Ordering::SeqCst);
            assert_eq!(
                api.core.reconcile(&submitted.id).await.unwrap().run.status,
                genaimage_core::RunStatus::Cancelled
            );
        } else {
            assert!(matches!(
                current.status,
                genaimage_core::RunStatus::Queued | genaimage_core::RunStatus::Running
            ));
            assert!(events
                .iter()
                .any(|event| event.event_type == "cancellation_not_sent"));
        }
        assert_eq!(
            api.fake.interrupt_calls.load(Ordering::SeqCst),
            usize::from(attempted)
        );
    }
}

#[tokio::test]
async fn rejected_deletion_rolls_back_but_lost_acknowledgement_stays_cancelling() {
    let api = ApiHarness::start_with_backend_timeout(1).await;
    install_workflow(&api);
    let submitted = run(
        submit(&api, "lost-delete-ack", &request()).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let path = format!("{RUNS}/{}", submitted.id);
    let cancel = || api.request(Method::POST, &format!("{path}/cancel")).send();
    api.fake.reject_delete.store(true, Ordering::SeqCst);
    ApiHarness::assert_problem(cancel().await.unwrap(), StatusCode::BAD_GATEWAY).await;
    let restored: RunResponse = api.get_json(&path).await;
    assert_eq!(restored.status, RunStatus::Queued);
    assert!(!api.fake.cancelled.load(Ordering::SeqCst));

    api.fake.reject_delete.store(false, Ordering::SeqCst);
    api.fake.hang_delete_response.store(true, Ordering::SeqCst);
    let problem =
        ApiHarness::assert_problem(cancel().await.unwrap(), StatusCode::BAD_GATEWAY).await;
    assert!(problem.detail.contains("outcome is unknown"));
    assert!(problem.recovery.contains("Reconcile"));
    assert!(api.fake.cancelled.load(Ordering::SeqCst));
    let uncertain: RunResponse = api.get_json(&path).await;
    assert_eq!(uncertain.status, RunStatus::Cancelling);
    let history: RunEventsResponse = api.get_json(&format!("{path}/events")).await;
    let unknown = history
        .events
        .iter()
        .find(|event| event.event_type == "cancellation_unknown")
        .unwrap();
    assert_eq!(unknown.payload["prompt_id"], submitted.prompt_id);
    let reconciled: RunResponse = api
        .post_json(&format!("{path}/reconcile"), &Value::Null)
        .await;
    assert_eq!(
        reconciled.status,
        RunStatus::Cancelling,
        "absence from queue and history does not confirm cancellation"
    );
    assert_eq!(api.fake.delete_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[tokio::test]
async fn resolved_workflow_verifies_submission_digest_and_supports_legacy_runs() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let workflows = &api.core.config().workflows;
    let graph_path = workflows.join("workflow.api.json");
    let mut graph: Value = serde_json::from_slice(&std::fs::read(&graph_path).unwrap()).unwrap();
    graph["9"]["inputs"]["images"] = json!(["3", 0]);
    std::fs::write(&graph_path, serde_json::to_vec(&graph).unwrap()).unwrap();
    let manifest_path = workflows.join("contract.genaimage.json");
    let mut workflow_manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    workflow_manifest["parameters"]["images"] =
        json!({"node":"9", "input":"images", "kind":"string"});
    workflow_manifest["parameters"]["prefix"] =
        json!({"node":"9", "input":"filename_prefix", "kind":"string"});
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&workflow_manifest).unwrap(),
    )
    .unwrap();
    let workflow: WorkflowDetails = api.get_json(WORKFLOW).await;
    let mut body = request();
    body["workflow_revision"] = json!(workflow.revision);
    let validation: Value = api.post_json(&format!("{WORKFLOW}/validate"), &json!({"workflow_revision": body["workflow_revision"], "preset": "fast", "parameters": {"seed": 99}})).await;
    assert_eq!(validation["valid"], true);
    assert_eq!(validation["revision"], body["workflow_revision"]);
    assert_eq!(
        validation["inherited_inputs"],
        json!([
            {"parameter":"images", "node":"9", "input":"images", "source":{"kind":"connection", "node":"3", "output":0}},
            {"parameter":"prefix", "node":"9", "input":"filename_prefix", "source":{"kind":"literal", "json":"\"genaimage\""}},
        ])
    );
    let source = run(
        submit(&api, "workflow-read", &body).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let path = format!("{RUNS}/{}/resolved-workflow", source.id);
    let record = api.core.repository().get(&source.id).unwrap();
    let digest = record.resolved_workflow_sha256.as_ref().unwrap();
    let graph_path = std::path::Path::new(&record.output_dir).join("resolved-workflow.json");
    let response = api.request(Method::GET, &path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-genaimage-workflow-integrity"],
        "verified"
    );
    assert_eq!(response.headers()["etag"], format!("\"{digest}\""));
    let graph: Value = response.json().await.unwrap();
    assert_eq!(
        graph,
        *api.fake.submitted_graph.lock().unwrap().as_ref().unwrap()
    );
    assert_eq!(graph["9"]["inputs"]["images"], json!(["3", 0]));
    assert_eq!(graph["9"]["inputs"]["filename_prefix"], "genaimage");
    api.fake.complete.store(true, Ordering::SeqCst);
    let completed: RunResponse = api
        .post_json(&format!("{RUNS}/{}/reconcile", source.id), &Value::Null)
        .await;
    assert_eq!(completed.status, RunStatus::Completed);
    let manifest: Value = api
        .get_json(&format!("{RUNS}/{}/manifest", source.id))
        .await;
    assert_eq!(manifest["resolved_workflow_sha256"], *digest);

    std::fs::write(&graph_path, br#"{"different":{"class_type":"Changed"}}"#).unwrap();
    let response = api.request(Method::GET, &path).send().await.unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    assert!(problem.recovery.contains("resolved workflow"));

    let db = rusqlite::Connection::open(&api.core.config().state_database).unwrap();
    db.execute(
        "UPDATE runs SET resolved_workflow_sha256 = NULL WHERE id = ?1",
        [&source.id],
    )
    .unwrap();
    drop(db);
    let response = api.request(Method::GET, &path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-genaimage-workflow-integrity"],
        "unverified"
    );
    let legacy: Value = response.json().await.unwrap();
    assert_eq!(legacy["different"]["class_type"], "Changed");

    let response = api
        .request(Method::GET, &format!("{RUNS}/missing/resolved-workflow"))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
    assert!(!problem.recovery.contains("completion manifest"));
    api.shutdown().await;
}
