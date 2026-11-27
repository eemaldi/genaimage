mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    BatchItemSubmission, BatchListResponse, BatchPreviewResponse, BatchResponse, BatchState,
    RunListResponse, RunResponse, RunStatus, WorkflowDetails,
};
use genaimage_core::Event;
use genaimage_server::{serve_on_listener, AppState};
use reqwest::{header, Method, Response, StatusCode};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    time::{sleep, Instant},
};

const BATCHES: &str = "/api/v1/batches";
const MAX_SEED: &str = "18446744073709551615";

/// The contract workflow plus an optional number parameter, to show exact numbers.
fn install_batch_workflow(api: &ApiHarness) {
    install_workflow(api);
    let root = &api.core.config().workflows;
    let mut graph: Value =
        serde_json::from_slice(&std::fs::read(root.join("workflow.api.json")).unwrap()).unwrap();
    graph["3"]["inputs"]["cfg"] = json!(7);
    std::fs::write(
        root.join("workflow.api.json"),
        serde_json::to_vec(&graph).unwrap(),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("contract.genaimage.json")).unwrap())
            .unwrap();
    manifest["parameters"]["cfg"] =
        json!({"node": "3", "input": "cfg", "kind": "number", "required": false});
    std::fs::write(
        root.join("contract.genaimage.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

async fn revision(api: &ApiHarness) -> Value {
    let details: WorkflowDetails = api.get_json("/api/v1/workflows/contract-workflow").await;
    json!(details.revision.unwrap())
}

async fn request(api: &ApiHarness, parameters: Value, sweep: Value, max_items: usize) -> Value {
    json!({
        "workflow": "contract-workflow",
        "workflow_revision": revision(api).await,
        "parameters": parameters,
        "sweep": sweep,
        "max_items": max_items
    })
}

async fn post(api: &ApiHarness, path: &str, key: Option<&str>, body: &Value) -> Response {
    let mut request = api.request(Method::POST, path);
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    request.json(body).send().await.unwrap()
}

async fn submit(api: &ApiHarness, key: &str, body: &Value) -> Response {
    post(api, BATCHES, Some(key), body).await
}

async fn batch(response: Response, expected: StatusCode) -> BatchResponse {
    assert_eq!(response.status(), expected);
    response.json().await.unwrap()
}

/// Waits until the driver is done submitting, not until the runs finish.
async fn settled(api: &ApiHarness, id: &str) -> BatchResponse {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current: BatchResponse = api.get_json(&format!("{BATCHES}/{id}")).await;
        if current.state != BatchState::Submitting {
            return current;
        }
        assert!(Instant::now() < deadline, "batch stayed submitting");
        sleep(Duration::from_millis(10)).await;
    }
}

fn text(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

#[tokio::test]
async fn preview_expands_a_grid_in_sorted_order_with_exact_values() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 18446744073709551615_u64}),
        json!({"grid": {"steps": [10, 20], "cfg": [2.0, 2.5, 3]}}),
        6,
    )
    .await;
    let response = post(&api, &format!("{BATCHES}/preview"), None, &body).await;
    assert_eq!(response.status(), StatusCode::OK);
    let preview: BatchPreviewResponse = response.json().await.unwrap();
    assert_eq!(
        (preview.item_count, preview.max_items, preview.max_items_cap),
        (6, 6, 256)
    );
    // Names sort to cfg, steps; steps varies fastest.
    let items: Vec<String> = preview
        .items
        .iter()
        .map(|item| text(&item.parameters))
        .collect();
    let seed = format!(r#""seed":{MAX_SEED}"#);
    assert_eq!(
        items,
        [
            format!(r#"{{"cfg":2.0,{seed},"steps":10}}"#),
            format!(r#"{{"cfg":2.0,{seed},"steps":20}}"#),
            format!(r#"{{"cfg":2.5,{seed},"steps":10}}"#),
            format!(r#"{{"cfg":2.5,{seed},"steps":20}}"#),
            format!(r#"{{"cfg":3,{seed},"steps":10}}"#),
            format!(r#"{{"cfg":3,{seed},"steps":20}}"#),
        ]
    );
    assert_eq!(preview.items[3].index, 3);
    assert_eq!(
        text(&preview.items[3].sweep_values),
        r#"{"cfg":2.5,"steps":20}"#
    );
    assert_eq!(
        text(&preview.items[3].effective_parameters),
        format!(r#"{{"cfg":2.5,{seed},"steps":20}}"#)
    );
    assert!(preview.inherited_inputs.is_empty());
    // A preview is read-only: nothing was submitted, recorded or even checked against ComfyUI.
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.fake.object_info_calls.load(Ordering::SeqCst), 0);
    let listed: BatchListResponse = api.get_json(BATCHES).await;
    assert!(listed.batches.is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn preview_shows_inherited_inputs_and_preset_values() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let mut body = request(
        &api,
        json!({}),
        json!({"seeds": {"parameter": "seed", "values": [1, 2]}}),
        2,
    )
    .await;
    body["preset"] = json!("fast");
    let preview: BatchPreviewResponse = post(&api, &format!("{BATCHES}/preview"), None, &body)
        .await
        .json()
        .await
        .unwrap();
    let item = &preview.items[0];
    assert_eq!(text(&item.parameters), r#"{"seed":1}"#);
    assert_eq!(text(&item.effective_parameters), r#"{"seed":1,"steps":10}"#);
    // Inherited inputs are shared by every item, so they appear once on the batch.
    assert_eq!(preview.items.len(), 2);
    assert_eq!(preview.inherited_inputs.len(), 1);
    assert_eq!(preview.inherited_inputs[0].parameter, "cfg");
    assert_eq!(
        text(&preview.inherited_inputs[0].source),
        r#"{"kind":"literal","json":"7"}"#
    );
    assert!(!text(item).contains("inherited_inputs"));
    api.shutdown().await;
}

#[tokio::test]
async fn limits_overlap_and_duplicates_are_rejected_never_truncated() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let preview = format!("{BATCHES}/preview");
    let too_many = request(
        &api,
        json!({}),
        json!({"grid": {"seed": [1, 2, 3], "steps": [4, 5]}}),
        5,
    )
    .await;
    let problem = ApiHarness::assert_typed_problem(
        post(&api, &preview, None, &too_many).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "urn:genaimage:problem:batch-too-large",
    )
    .await;
    assert!(problem.detail.contains("6 items") && problem.detail.contains("max_items 5"));

    let seeds: Vec<u32> = (0..257).collect();
    let above_cap = request(
        &api,
        json!({"steps": 5}),
        json!({"seeds": {"parameter": "seed", "values": seeds}}),
        1000,
    )
    .await;
    let problem = ApiHarness::assert_typed_problem(
        submit(&api, "cap", &above_cap).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "urn:genaimage:problem:batch-too-large",
    )
    .await;
    assert!(problem.detail.contains("257") && problem.detail.contains("256"));

    for (parameters, sweep, needle) in [
        (
            json!({"seed": 5}),
            json!({"grid": {"seed": [1, 2]}}),
            "also set in parameters",
        ),
        (
            json!({"steps": 5}),
            json!({"seeds": {"parameter": "seed", "values": [1, 2, 1]}}),
            "positions 0 and 2",
        ),
        (
            json!({}),
            json!({"grid": {"cfg": [2, 2.0], "seed": [1]}}),
            "positions 0 and 1 are numerically equal",
        ),
        (
            json!({"seed": 1}),
            json!({"grid": {"missing": [1]}}),
            "not defined",
        ),
        (
            json!({}),
            json!({"seeds": {"parameter": "steps", "values": [1]}}),
            "not a seed",
        ),
    ] {
        let body = request(&api, parameters, sweep, 10).await;
        let problem = ApiHarness::assert_typed_problem(
            post(&api, &preview, None, &body).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "urn:genaimage:problem:batch-invalid",
        )
        .await;
        assert!(problem.detail.contains(needle), "{}", problem.detail);
    }

    // A grid that names a parameter twice is rejected, never merged: the later
    // list must not silently replace the earlier one.
    let repeated = format!(
        r#"{{"workflow":"contract-workflow","workflow_revision":{},"max_items":10,
            "sweep":{{"grid":{{"steps":[1,2],"seed":[1],"steps":[3]}}}}}}"#,
        revision(&api).await
    );
    for path in [preview.as_str(), BATCHES] {
        let mut builder = api
            .request(Method::POST, path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(repeated.clone());
        if path == BATCHES {
            builder = builder.header("Idempotency-Key", "repeated");
        }
        let problem = ApiHarness::assert_typed_problem(
            builder.send().await.unwrap(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "urn:genaimage:problem:batch-invalid",
        )
        .await;
        assert!(problem.detail.contains("'steps'"), "{}", problem.detail);
    }

    // Malformed shapes never reach validation: max_items and the pinned revision are required.
    for body in [
        json!({"workflow": "contract-workflow", "sweep": {"grid": {"seed": [1]}}, "max_items": 1}),
        json!({"workflow": "contract-workflow", "workflow_revision": "x", "sweep": {"grid": {"seed": [1]}}}),
        json!({"workflow": "contract-workflow", "workflow_revision": "x", "max_items": 1, "sweep": {"grid": {"seed": [1]}, "seeds": {"parameter": "seed", "values": [1]}}}),
    ] {
        let response = post(&api, &preview, None, &body).await;
        assert!(response.status().is_client_error(), "{body}");
    }
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    api.shutdown().await;
}

#[tokio::test]
async fn one_invalid_item_rejects_the_batch_and_lists_it_by_index() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [10, 0, 20, 101]}}),
        10,
    )
    .await;
    let response = submit(&api, "invalid", &body).await;
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "urn:genaimage:problem:batch-items-invalid",
    )
    .await;
    assert_eq!(problem.invalid_item_count, Some(2));
    assert_eq!(
        problem
            .invalid_items
            .iter()
            .map(|item| item.index)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(problem.invalid_items[0].errors[0].parameter, "steps");
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api
        .get_json::<BatchListResponse>(BATCHES)
        .await
        .batches
        .is_empty());
    assert!(api
        .get_json::<RunListResponse>("/api/v1/runs")
        .await
        .runs
        .is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn a_batch_submits_ordinary_runs_with_exact_numbers_and_batch_links() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let mut events = api.subscribe();
    let body = request(
        &api,
        json!({"steps": 20, "cfg": 2.0}),
        json!({"seeds": {"parameter": "seed", "values": [18446744073709551615_u64, 0, 9007199254740993_u64]}}),
        3,
    )
    .await;
    let response = submit(&api, "exact", &body).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let created: BatchResponse = response.json().await.unwrap();
    assert_eq!(created.item_count, 3);
    let id = created.id.clone();
    let batch = settled(&api, &id).await;
    assert_eq!(batch.state, BatchState::Submitted);
    assert_eq!(batch.counts.submitted, 3);
    assert_eq!(batch.items.len(), 3);
    for (index, item) in batch.items.iter().enumerate() {
        assert_eq!(item.index, index);
        assert_eq!(item.submission, BatchItemSubmission::Submitted);
        assert_eq!(item.run_status, Some(RunStatus::Queued));
        assert!(item.run_id.is_some() && item.error.is_none());
    }
    assert_eq!(
        text(&batch.items[0].parameters),
        format!(r#"{{"cfg":2.0,"seed":{MAX_SEED},"steps":20}}"#)
    );
    assert_eq!(
        text(&batch.sweep),
        format!(r#"{{"seeds":{{"parameter":"seed","values":[{MAX_SEED},0,9007199254740993]}}}}"#)
    );

    let graphs = api.fake.submitted_graphs.lock().unwrap().clone();
    let inputs: Vec<String> = graphs
        .iter()
        .map(|graph| text(&graph["3"]["inputs"]))
        .collect();
    assert_eq!(
        inputs,
        [
            format!(r#"{{"cfg":2.0,"seed":{MAX_SEED},"steps":20}}"#),
            r#"{"cfg":2.0,"seed":0,"steps":20}"#.to_owned(),
            r#"{"cfg":2.0,"seed":9007199254740993,"steps":20}"#.to_owned(),
        ]
    );

    // Each item is a run that knows its batch; the runs list filters by it.
    let run: RunResponse = api
        .get_json(&format!(
            "/api/v1/runs/{}",
            batch.items[2].run_id.as_ref().unwrap()
        ))
        .await;
    assert_eq!(
        (run.batch_id.as_deref(), run.batch_index),
        (Some(id.as_str()), Some(2))
    );
    let filtered: RunListResponse = api.get_json(&format!("/api/v1/runs?batch={id}")).await;
    let mut listed: Vec<u32> = filtered
        .runs
        .iter()
        .filter_map(|run| run.batch_index)
        .collect();
    listed.sort_unstable();
    assert_eq!(listed, [0, 1, 2]);
    let other: RunListResponse = api.get_json("/api/v1/runs?batch=batch-unknown").await;
    assert!(other.runs.is_empty());
    // Run events carry the batch fields, so the portal can group live updates.
    for expected in 0..3 {
        let event = ApiHarness::await_event(&mut events, Duration::from_secs(5), |event| {
            matches!(&event.event, Event::RunQueued { run } if run.batch_index == Some(expected))
        })
        .await;
        let Event::RunQueued { run } = event.event else {
            unreachable!()
        };
        assert_eq!(run.batch_id.as_deref(), Some(id.as_str()));
        assert_eq!(serde_json::to_value(&run).unwrap()["batch_index"], expected);
    }

    let listed: BatchListResponse = api.get_json(BATCHES).await;
    assert_eq!(listed.batches.len(), 1);
    assert_eq!(listed.batches[0].id, id);
    assert_eq!(listed.batches[0].state, BatchState::Submitted);
    assert_eq!(listed.batches[0].counts.submitted, 3);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 3);
    api.shutdown().await;
}

#[tokio::test]
async fn the_same_key_replays_and_a_different_body_conflicts() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [4, 5]}}),
        2,
    )
    .await;
    let response = submit(&api, "replay", &body).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let first: BatchResponse = response.json().await.unwrap();
    assert_eq!(location, format!("/api/v1/batches/{}", first.id));
    settled(&api, &first.id).await;

    let response = submit(&api, "replay", &body).await;
    assert_eq!(response.headers()[header::LOCATION], location.as_str());
    let replay = batch(response, StatusCode::OK).await;
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.state, BatchState::Submitted);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);

    let mut different = body.clone();
    different["sweep"] = json!({"grid": {"steps": [4, 6]}});
    ApiHarness::assert_problem(
        submit(&api, "replay", &different).await,
        StatusCode::CONFLICT,
    )
    .await;
    let mut spelt = body.clone();
    spelt["parameters"]["cfg"] = json!(2.0);
    let mut integer = body.clone();
    integer["parameters"]["cfg"] = json!(2);
    assert_eq!(
        submit(&api, "spelling", &spelt).await.status(),
        StatusCode::ACCEPTED
    );
    ApiHarness::assert_problem(
        submit(&api, "spelling", &integer).await,
        StatusCode::CONFLICT,
    )
    .await;

    let missing = post(&api, BATCHES, None, &body).await;
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    api.shutdown().await;
}

#[tokio::test]
async fn an_ambiguous_item_is_reported_unknown_reconciled_and_never_resubmitted() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    api.fake.fail_submit_on_call.store(2, Ordering::SeqCst);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2, 3]}}),
        3,
    )
    .await;
    let created = batch(submit(&api, "unknown", &body).await, StatusCode::ACCEPTED).await;
    let batch_state = settled(&api, &created.id).await;
    let submissions: Vec<_> = batch_state
        .items
        .iter()
        .map(|item| item.submission)
        .collect();
    assert_eq!(
        submissions,
        [
            BatchItemSubmission::Submitted,
            BatchItemSubmission::OutcomeUnknown,
            BatchItemSubmission::Submitted
        ]
    );
    assert_eq!(batch_state.state, BatchState::Partial);
    assert_eq!(batch_state.counts.outcome_unknown, 1);
    assert_eq!(
        batch_state.items[1].run_status,
        Some(RunStatus::UnknownSubmission)
    );
    assert!(batch_state.items[1].error.is_some());
    assert!(batch_state.items[0].error.is_none());

    // Neither a resume nor a replay sends it again.
    let resumed = post(
        &api,
        &format!("{BATCHES}/{}/resume", created.id),
        None,
        &json!({}),
    )
    .await;
    let resumed = batch(resumed, StatusCode::OK).await;
    assert_eq!(resumed.counts.outcome_unknown, 1);
    assert_eq!(
        batch(submit(&api, "unknown", &body).await, StatusCode::OK)
            .await
            .id,
        created.id
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 3);

    // Reconciliation finds the prompt in ComfyUI's queue.
    let run_id = batch_state.items[1].run_id.clone().unwrap();
    let run: RunResponse = api.get_json(&format!("/api/v1/runs/{run_id}")).await;
    *api.fake.queue_override.lock().unwrap() = Some(json!({
        "queue_running": [],
        "queue_pending": [[1, run.prompt_id, {}, {}]]
    }));
    let reconciled = api
        .request(Method::POST, &format!("/api/v1/runs/{run_id}/reconcile"))
        .send()
        .await
        .unwrap();
    assert_eq!(reconciled.status(), StatusCode::OK);
    let after: BatchResponse = api.get_json(&format!("{BATCHES}/{}", created.id)).await;
    assert_eq!(after.state, BatchState::Submitted);
    assert_eq!(after.items[1].submission, BatchItemSubmission::Submitted);
    assert_eq!(after.items[1].run_status, Some(RunStatus::Queued));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 3);
    api.shutdown().await;
}

#[tokio::test]
async fn a_changed_workflow_blocks_the_pinned_batch_before_anything_is_sent() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2]}}),
        2,
    )
    .await;
    let graph = api.core.config().workflows.join("workflow.api.json");
    let mut changed: Value = serde_json::from_slice(&std::fs::read(&graph).unwrap()).unwrap();
    changed["9"]["inputs"]["filename_prefix"] = json!("changed");
    std::fs::write(&graph, serde_json::to_vec(&changed).unwrap()).unwrap();
    ApiHarness::assert_typed_problem(
        submit(&api, "stale", &body).await,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:workflow-changed",
    )
    .await;
    ApiHarness::assert_typed_problem(
        post(&api, &format!("{BATCHES}/preview"), None, &body).await,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:workflow-changed",
    )
    .await;
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api
        .get_json::<BatchListResponse>(BATCHES)
        .await
        .batches
        .is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn unknown_batches_are_not_found_and_a_missing_node_rejects_before_recording() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    ApiHarness::assert_problem(
        api.request(Method::GET, &format!("{BATCHES}/batch-nope"))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    ApiHarness::assert_problem(
        post(
            &api,
            &format!("{BATCHES}/batch-nope/resume"),
            None,
            &json!({}),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    api.fake
        .missing_classes
        .lock()
        .unwrap()
        .push("KSampler".into());
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2]}}),
        2,
    )
    .await;
    ApiHarness::assert_problem(
        submit(&api, "nodes", &body).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api
        .get_json::<BatchListResponse>(BATCHES)
        .await
        .batches
        .is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn batches_list_newest_first_with_a_stable_cursor_and_a_workflow_filter() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let mut ids = Vec::new();
    for (key, steps) in [("one", 1), ("two", 2), ("three", 3)] {
        let body = request(
            &api,
            json!({"seed": 1}),
            json!({"grid": {"steps": [steps]}}),
            1,
        )
        .await;
        let created = batch(submit(&api, key, &body).await, StatusCode::ACCEPTED).await;
        settled(&api, &created.id).await;
        ids.push(created.id);
        sleep(Duration::from_millis(5)).await;
    }
    let first: BatchListResponse = api.get_json(&format!("{BATCHES}?limit=2")).await;
    assert_eq!(
        first
            .batches
            .iter()
            .map(|batch| batch.id.as_str())
            .collect::<Vec<_>>(),
        [ids[2].as_str(), ids[1].as_str()]
    );
    let cursor = first.next_cursor.expect("another page follows");
    let second: BatchListResponse = api
        .get_json(&format!("{BATCHES}?limit=2&cursor={cursor}"))
        .await;
    assert_eq!(
        second
            .batches
            .iter()
            .map(|batch| batch.id.as_str())
            .collect::<Vec<_>>(),
        [ids[0].as_str()]
    );
    assert!(second.next_cursor.is_none());
    let none: BatchListResponse = api.get_json(&format!("{BATCHES}?workflow=other")).await;
    assert!(none.batches.is_empty());
    for bad in ["limit=0", "limit=201", "cursor=garbage"] {
        let response = api
            .request(Method::GET, &format!("{BATCHES}?{bad}"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error(), "{bad}");
    }
    api.shutdown().await;
}

#[tokio::test]
async fn starting_a_server_marks_an_abandoned_batch_interrupted_and_resume_submits_it() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2]}}),
        2,
    )
    .await;
    let creation = api
        .core
        .create_batch(
            "abandoned".into(),
            serde_json::from_value(body.clone()).unwrap(),
        )
        .await
        .unwrap();
    let id = creation.batch.record.id.clone();
    // The process died before its driver sent anything: only the claim is gone.
    drop(creation);
    let before: BatchResponse = api.get_json(&format!("{BATCHES}/{id}")).await;
    assert_eq!(before.state, BatchState::Submitting);

    // A new server starts over the same state and records the interruption.
    let state = AppState::new(api.core.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = {
        let state = state.clone();
        tokio::spawn(async move {
            serve_on_listener(
                state.clone(),
                listener,
                state.wait_for_shutdown(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let interrupted = loop {
        let batch: BatchResponse = api.get_json(&format!("{BATCHES}/{id}")).await;
        if batch.state != BatchState::Submitting || Instant::now() > deadline {
            break batch;
        }
        sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(interrupted.state, BatchState::Interrupted);
    assert_eq!(interrupted.counts.not_submitted, 2);
    assert!(interrupted.items.iter().all(|item| item.run_id.is_none()
        && item
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Resume the batch"))));
    assert_eq!(
        api.fake.prompt_calls.load(Ordering::SeqCst),
        0,
        "recovery never resumes"
    );
    state.begin_shutdown();
    server.await.unwrap();

    // Resuming by ID needs no key and submits each item once.
    let resumed = post(&api, &format!("{BATCHES}/{id}/resume"), None, &json!({})).await;
    assert_eq!(resumed.status(), StatusCode::ACCEPTED);
    let done = settled(&api, &id).await;
    assert_eq!(done.state, BatchState::Submitted);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    let again = post(&api, &format!("{BATCHES}/{id}/resume"), None, &json!({})).await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[tokio::test]
async fn negative_zero_survives_the_request_storage_the_prompt_and_every_read() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = format!(
        r#"{{"workflow":"contract-workflow","workflow_revision":{},"max_items":2,
            "parameters":{{"seed":1,"steps":4}},"sweep":{{"grid":{{"cfg":[-0.0,2.50]}}}}}}"#,
        revision(&api).await
    );
    let send = |path: String, key: Option<&'static str>| {
        let mut builder = api
            .request(Method::POST, &path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        if let Some(key) = key {
            builder = builder.header("Idempotency-Key", key);
        }
        async move { builder.send().await.unwrap() }
    };
    let preview: Value = send(format!("{BATCHES}/preview"), None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        text(&preview["items"][0]["parameters"]["cfg"]),
        "-0.0",
        "{preview}"
    );
    assert_eq!(text(&preview["items"][1]["parameters"]["cfg"]), "2.50");

    let created = batch(
        send(BATCHES.to_owned(), Some("negative-zero")).await,
        StatusCode::ACCEPTED,
    )
    .await;
    let done = settled(&api, &created.id).await;
    assert_eq!(done.state, BatchState::Submitted);
    let cfgs = |batch: &BatchResponse| -> Vec<String> {
        batch
            .items
            .iter()
            .map(|item| text(&item.parameters["cfg"]))
            .collect()
    };
    assert_eq!(cfgs(&done), ["-0.0", "2.50"]);
    // The stored request, the replay and a plain read all agree.
    assert_eq!(text(&done.sweep), r#"{"grid":{"cfg":[-0.0,2.50]}}"#);
    let replay = batch(
        send(BATCHES.to_owned(), Some("negative-zero")).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(cfgs(&replay), ["-0.0", "2.50"]);
    let read: BatchResponse = api.get_json(&format!("{BATCHES}/{}", created.id)).await;
    assert_eq!(cfgs(&read), ["-0.0", "2.50"]);

    // ComfyUI received the same spelling, and so did the recorded run.
    let sent: Vec<String> = api
        .fake
        .submitted_graphs
        .lock()
        .unwrap()
        .iter()
        .map(|graph| text(&graph["3"]["inputs"]["cfg"]))
        .collect();
    assert_eq!(sent, ["-0.0", "2.50"]);
    let run: RunResponse = api
        .get_json(&format!(
            "/api/v1/runs/{}",
            done.items[0].run_id.as_ref().unwrap()
        ))
        .await;
    assert_eq!(text(&run.parameters["cfg"]), "-0.0");
    api.shutdown().await;
}

#[tokio::test]
async fn a_client_that_disconnects_while_the_batch_is_created_still_gets_it_driven() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2]}}),
        2,
    )
    .await;
    // The node check of the creation is slow, and the client gives up during it.
    api.fake.object_info_delay_ms.store(400, Ordering::SeqCst);
    let outcome = api
        .request(Method::POST, BATCHES)
        .header("Idempotency-Key", "gone")
        .timeout(Duration::from_millis(100))
        .json(&body)
        .send()
        .await;
    assert!(outcome.unwrap_err().is_timeout());

    let deadline = Instant::now() + Duration::from_secs(5);
    let id = loop {
        let listed: BatchListResponse = api.get_json(BATCHES).await;
        if let Some(created) = listed.batches.first() {
            break created.id.clone();
        }
        assert!(Instant::now() < deadline, "the batch was never created");
        sleep(Duration::from_millis(10)).await;
    };
    // Nobody received the batch, yet its driver ran: it is not stuck submitting.
    let done = settled(&api, &id).await;
    assert_eq!(done.state, BatchState::Submitted);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[tokio::test]
async fn resume_and_replay_while_a_driver_runs_start_no_second_driver() {
    let api = ApiHarness::start().await;
    install_batch_workflow(&api);
    let body = request(
        &api,
        json!({"seed": 1}),
        json!({"grid": {"steps": [1, 2, 3]}}),
        3,
    )
    .await;
    // The first prompt is in flight, so the driver holds its claim.
    api.fake.hold_prompts.store(true, Ordering::SeqCst);
    let created = batch(submit(&api, "busy", &body).await, StatusCode::ACCEPTED).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while api.fake.prompt_calls.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the driver never sent an item");
        sleep(Duration::from_millis(10)).await;
    }

    let resume_path = format!("{BATCHES}/{}/resume", created.id);
    let empty = json!({});
    let (first, second, third, fourth) = tokio::join!(
        post(&api, &resume_path, None, &empty),
        submit(&api, "busy", &body),
        post(&api, &resume_path, None, &empty),
        submit(&api, "busy", &body),
    );
    for response in [first, second, third, fourth] {
        let current = batch(response, StatusCode::OK).await;
        assert_eq!(current.id, created.id);
        assert_eq!(current.state, BatchState::Submitting);
    }
    sleep(Duration::from_millis(150)).await;
    assert_eq!(
        api.fake.prompt_calls.load(Ordering::SeqCst),
        1,
        "only the first driver sends"
    );

    api.fake.hold_prompts.store(false, Ordering::SeqCst);
    let done = settled(&api, &created.id).await;
    assert_eq!(done.state, BatchState::Submitted);
    let sent: Vec<String> = api
        .fake
        .submitted_graphs
        .lock()
        .unwrap()
        .iter()
        .map(|graph| text(&graph["3"]["inputs"]["steps"]))
        .collect();
    assert_eq!(sent, ["1", "2", "3"]);
    let mut run_ids: Vec<_> = done
        .items
        .iter()
        .filter_map(|item| item.run_id.clone())
        .collect();
    run_ids.sort();
    run_ids.dedup();
    assert_eq!(run_ids.len(), 3);
    api.shutdown().await;
}
