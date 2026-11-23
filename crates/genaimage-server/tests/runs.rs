mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    ReconcileRunsResponse, RunEventsResponse, RunListResponse, RunResponse, RunStatus,
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
    let reconcile = api
        .request(Method::POST, &format!("{path}/reconcile"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        run(reconcile, StatusCode::OK).await.status,
        RunStatus::Queued
    );
    api.fake.complete.store(true, Ordering::SeqCst);
    let bulk: ReconcileRunsResponse = api
        .post_json(&format!("{RUNS}/reconcile"), &Value::Null)
        .await;
    assert_eq!(bulk.runs.len(), 1);
    assert_eq!(bulk.runs[0].id, submitted.id);
    assert_eq!(bulk.runs[0].status, RunStatus::Completed);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    api.shutdown().await;
}

#[tokio::test]
async fn cancels_pending_and_executing_prompts_with_the_appropriate_backend_action() {
    for running in [false, true] {
        let api = ApiHarness::start().await;
        install_workflow(&api);
        api.fake.running.store(running, Ordering::SeqCst);
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
