mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    ReconcileOutcome, ReconcileRunResponse, ReconcileRunsResponse, RunEventsResponse,
    RunListResponse, RunResponse, RunStatus,
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
    let bulk: ReconcileRunsResponse = api
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
    for (history, expected) in [
        (
            json!({submitted.prompt_id.clone(): {"status": {"completed": false}}}),
            ReconcileOutcome::StillAmbiguous,
        ),
        (json!(null), ReconcileOutcome::StillAmbiguous),
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
    let bulk: ReconcileRunsResponse = api
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
