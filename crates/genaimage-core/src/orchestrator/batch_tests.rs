use std::{collections::BTreeSet, sync::Mutex};

use serde_json::json;
use tempfile::TempDir;
use tokio::{
    sync::{mpsc, Semaphore},
    time::timeout,
};

use super::*;
use crate::{
    async_trait,
    batch::{BatchRequest, BatchSummary, BatchView, ItemSubmission},
    image_backend::{
        EventMonitor, HistorySnapshot, OutputReference, PromptResponse, QueueSnapshot,
        UploadResponse,
    },
    HarnessConfig,
};

struct Gate {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}

#[derive(Default)]
struct Script {
    /// Every prompt sent, in order, whether or not the response was lost.
    prompts: Vec<(Value, String)>,
    /// 1-based submit calls whose response is lost after the prompt was recorded.
    lose_response_on: BTreeSet<usize>,
    panic_on_submit: Option<usize>,
    /// 1-based `object_info` calls; call 1 is the batch's one-off node check.
    object_info_calls: usize,
    object_info_fails_on: BTreeSet<usize>,
    submit_gate: Option<(usize, Arc<Gate>)>,
}

#[derive(Clone, Default)]
struct ScriptBackend(Arc<Mutex<Script>>);

#[async_trait]
impl ImageBackend for ScriptBackend {
    fn name(&self) -> &str {
        "scripted backend"
    }
    fn base_url(&self) -> &str {
        "fake://script"
    }
    async fn health(&self) -> Result<BackendHealth> {
        Ok(BackendHealth { system: json!({}) })
    }
    async fn object_info(&self) -> Result<Value> {
        let mut script = self.0.lock().unwrap();
        script.object_info_calls += 1;
        if script
            .object_info_fails_on
            .contains(&script.object_info_calls)
        {
            return Err(HarnessError::Backend {
                backend: "scripted backend".into(),
                message: "object_info unavailable".into(),
            });
        }
        Ok(json!({"KSampler": {}, "SaveImage": {}}))
    }
    async fn queue(&self) -> Result<QueueSnapshot> {
        let pending = self
            .0
            .lock()
            .unwrap()
            .prompts
            .iter()
            .map(|(_, id)| id.clone())
            .collect();
        Ok(QueueSnapshot {
            running: vec![],
            pending,
            raw: Value::Null,
        })
    }
    async fn submit(&self, workflow: &Value, _: &str, prompt_id: &str) -> Result<PromptResponse> {
        let (gate, lose, panics) = {
            let mut script = self.0.lock().unwrap();
            script
                .prompts
                .push((workflow.clone(), prompt_id.to_owned()));
            let call = script.prompts.len();
            let gate = script
                .submit_gate
                .take_if(|(on, _)| *on == call)
                .map(|(_, gate)| gate);
            (
                gate,
                script.lose_response_on.contains(&call),
                script.panic_on_submit == Some(call),
            )
        };
        assert!(!panics, "scripted submit panic");
        if let Some(gate) = gate {
            gate.entered.send(()).unwrap();
            gate.release.acquire().await.unwrap().forget();
        }
        if lose {
            return Err(HarnessError::Backend {
                backend: "scripted backend".into(),
                message: "response lost".into(),
            });
        }
        Ok(PromptResponse {
            prompt_id: prompt_id.to_owned(),
            number: None,
            node_errors: Value::Null,
        })
    }
    async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot> {
        Ok(HistorySnapshot {
            prompt_id: prompt_id.to_owned(),
            outcome: HistoryOutcome::Unresolved,
            outputs: vec![],
            raw: Value::Null,
        })
    }
    async fn delete_queued(&self, _: &str) -> Result<Value> {
        unreachable!("batches never cancel")
    }
    async fn interrupt(&self, _: &str) -> Result<Value> {
        unreachable!("batches never cancel")
    }
    async fn upload_image(&self, _: &Path) -> Result<UploadResponse> {
        unreachable!("the contract workflow has no image inputs")
    }
    async fn download_output(&self, _: &OutputReference) -> Result<Vec<u8>> {
        unreachable!("runs here never complete")
    }
    async fn monitor_events(&self, _: &str) -> Result<EventMonitor> {
        Err(HarnessError::UnsupportedOperation("no events".into()))
    }
}

struct Fixture {
    _workspace: TempDir,
    harness: Harness,
    script: Arc<Mutex<Script>>,
}

impl Fixture {
    fn new() -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let mut config =
            HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| {
                matches!(key, "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME")
                    .then(|| workspace.path().display().to_string())
            })
            .unwrap();
        config.state_database = workspace.path().join("state.sqlite3");
        config.runs = workspace.path().join("runs");
        fs::create_dir_all(&config.workflows).unwrap();
        let mut graph: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/contract-workflow.api.json"
        ))
        .unwrap();
        graph["3"]["inputs"]["cfg"] = json!(7);
        fs::write(
            config.workflows.join("workflow.api.json"),
            serde_json::to_vec(&graph).unwrap(),
        )
        .unwrap();
        let mut manifest: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/contract-workflow.genaimage.json"
        ))
        .unwrap();
        manifest["parameters"]["cfg"] =
            json!({"node": "3", "input": "cfg", "kind": "number", "required": false});
        manifest["parameters"]["prefix"] =
            json!({"node": "9", "input": "filename_prefix", "kind": "string", "required": false});
        fs::write(
            config.workflows.join("contract.genaimage.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let backend = ScriptBackend::default();
        let script = backend.0.clone();
        let harness = Harness::new(config, Arc::new(backend)).unwrap();
        Self {
            _workspace: workspace,
            harness,
            script,
        }
    }

    fn revision(&self) -> String {
        self.harness
            .load_workflow_by_id("contract-workflow")
            .unwrap()
            .unwrap()
            .revision()
    }

    fn request(&self, base: Value, sweep: Value) -> BatchRequest {
        serde_json::from_value(json!({
            "workflow": "contract-workflow",
            "workflow_revision": self.revision(),
            "parameters": base,
            "sweep": sweep,
            "max_items": 10
        }))
        .unwrap()
    }

    fn submitted_calls(&self) -> usize {
        self.script.lock().unwrap().prompts.len()
    }

    fn observer() -> RunObserver {
        Arc::new(|_| {})
    }

    /// Creates a batch and drives it to the end of its driver.
    async fn run_batch(&self, key: &str, request: BatchRequest) -> BatchView {
        let creation = self
            .harness
            .create_batch(key.into(), request)
            .await
            .unwrap();
        assert!(!creation.replayed);
        let id = creation.batch.record.id.clone();
        creation
            .driver
            .expect("a new batch has a driver")
            .run(
                CancellationToken::new(),
                CancellationToken::new(),
                Self::observer(),
            )
            .await;
        self.harness.get_batch(&id).await.unwrap()
    }
}

fn submissions(batch: &BatchView) -> Vec<ItemSubmission> {
    batch.items.iter().map(|item| item.submission).collect()
}

fn seeds(values: Value) -> Value {
    json!({"seeds": {"parameter": "seed", "values": values}})
}

#[tokio::test]
async fn items_are_ordinary_runs_with_exact_numbers_in_the_submitted_prompt() {
    let fixture = Fixture::new();
    let request = fixture.request(
        json!({"steps": 20, "cfg": 2.0}),
        seeds(json!([18446744073709551615_u64, 0, 9007199254740993_u64])),
    );
    let batch = fixture.run_batch("exact", request).await;
    assert_eq!(batch.state, BatchSummary::Submitted);
    assert_eq!(batch.record.item_count, 3);
    let script = fixture.script.lock().unwrap();
    let prompts: Vec<String> = script
        .prompts
        .iter()
        .map(|(prompt, _)| serde_json::to_string(&prompt["3"]["inputs"]).unwrap())
        .collect();
    assert_eq!(
        prompts,
        [
            r#"{"cfg":2.0,"seed":18446744073709551615,"steps":20}"#,
            r#"{"cfg":2.0,"seed":0,"steps":20}"#,
            r#"{"cfg":2.0,"seed":9007199254740993,"steps":20}"#,
        ]
    );
    drop(script);
    for (index, item) in batch.items.iter().enumerate() {
        let run = fixture
            .harness
            .get_run(item.run_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(run.batch_id.as_deref(), Some(batch.record.id.as_str()));
        assert_eq!(run.batch_index, Some(index as u32));
        assert_eq!(run.status, RunStatus::Queued);
        assert_eq!(item.run_status, Some(RunStatus::Queued));
    }
    assert_eq!(
        serde_json::to_string(&batch.items[0].parameters).unwrap(),
        r#"{"cfg":2.0,"seed":18446744073709551615,"steps":20}"#
    );
}

#[tokio::test]
async fn an_item_has_the_fingerprint_of_the_same_request_submitted_normally() {
    let fixture = Fixture::new();
    let revision = fixture.revision();
    let batch = fixture
        .run_batch(
            "fingerprint",
            fixture.request(json!({"steps": 5}), seeds(json!([7, 8]))),
        )
        .await;
    let normal = RunRequest {
        workflow: "contract-workflow".into(),
        preset: None,
        parameters: BTreeMap::from([("seed".into(), json!(8)), ("steps".into(), json!(5))]),
        version: None,
        workflow_revision: Some(revision),
    };
    let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&normal).unwrap()));
    let key = super::batches::batch_item_key(&batch.record.id, 1);
    let found = fixture
        .harness
        .repository
        .find_submission(&key, &fingerprint)
        .unwrap()
        .expect("the item key maps to the run of the equivalent normal request");
    assert_eq!(Some(found.id), batch.items[1].run_id);
    // Unpinned run requests serialise exactly as before batches existed.
    let unpinned = RunRequest {
        workflow_revision: None,
        ..normal
    };
    assert_eq!(
        serde_json::to_string(&unpinned).unwrap(),
        r#"{"workflow":"contract-workflow","preset":null,"parameters":{"seed":8,"steps":5},"version":null}"#
    );
}

#[tokio::test]
async fn one_invalid_item_submits_nothing_and_lists_each_invalid_index() {
    let fixture = Fixture::new();
    let request = fixture.request(
        json!({"seed": 1}),
        json!({"grid": {"steps": [10, 0, 20, 101]}}),
    );
    let error = fixture
        .harness
        .create_batch("invalid".into(), request.clone())
        .await
        .err()
        .unwrap();
    let HarnessError::BatchItemsInvalid { total, items } = error else {
        panic!("expected invalid items, got {error}");
    };
    assert_eq!(total, 2);
    assert_eq!(
        items.iter().map(|item| item.index).collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(items[0].errors[0].parameter, "steps");
    assert!(items[0].errors[0].reason.contains("below minimum"));
    assert!(items[1].errors[0].reason.contains("above maximum"));
    assert_eq!(fixture.submitted_calls(), 0);
    assert!(fixture.harness.repository.list(10).unwrap().is_empty());
    assert!(fixture
        .harness
        .list_batches(None, None, None)
        .await
        .unwrap()
        .0
        .is_empty());
    // The same key is still free: no reservation was made for the rejected batch.
    let fixed = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [10, 20]}}));
    assert_eq!(
        fixture.run_batch("invalid", fixed).await.state,
        BatchSummary::Submitted
    );
}

#[tokio::test]
async fn a_failed_item_does_not_stop_later_items_and_is_reported_per_item() {
    let fixture = Fixture::new();
    // Call 1 is the batch's node check; calls 2, 3 and 4 belong to items 0, 1 and 2.
    fixture
        .script
        .lock()
        .unwrap()
        .object_info_fails_on
        .insert(3);
    let batch = fixture
        .run_batch(
            "partial",
            fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2, 3]}})),
        )
        .await;
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::Failed,
            ItemSubmission::Submitted
        ]
    );
    assert_eq!(batch.state, BatchSummary::Partial);
    assert_eq!(batch.items[1].run_id, None);
    assert!(batch.items[1]
        .error
        .as_deref()
        .unwrap()
        .contains("object_info unavailable"));
    assert_eq!(batch.items[1].run_status, None);
    assert_eq!(fixture.submitted_calls(), 2);

    // Resuming retries only the item that never had a run.
    let resumed = fixture
        .harness
        .resume_batch(&batch.record.id)
        .await
        .unwrap();
    resumed
        .driver
        .unwrap()
        .run(
            CancellationToken::new(),
            CancellationToken::new(),
            Fixture::observer(),
        )
        .await;
    let batch = fixture.harness.get_batch(&batch.record.id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Submitted);
    assert_eq!(fixture.submitted_calls(), 3);
    let steps: Vec<Value> = fixture
        .script
        .lock()
        .unwrap()
        .prompts
        .iter()
        .map(|(prompt, _)| prompt["3"]["inputs"]["steps"].clone())
        .collect();
    assert_eq!(steps, [json!(1), json!(3), json!(2)]);
}

#[tokio::test]
async fn an_ambiguous_item_is_unknown_then_reconciled_and_never_resubmitted() {
    let fixture = Fixture::new();
    fixture.script.lock().unwrap().lose_response_on.insert(2);
    let batch = fixture
        .run_batch(
            "ambiguous",
            fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2, 3]}})),
        )
        .await;
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::OutcomeUnknown,
            ItemSubmission::Submitted
        ]
    );
    assert_eq!(
        batch.state,
        BatchSummary::Partial,
        "an unknown item is never a success"
    );
    assert_eq!(
        batch.items[1].run_status,
        Some(RunStatus::UnknownSubmission)
    );
    assert!(batch.items[1]
        .error
        .as_deref()
        .unwrap()
        .contains("response lost"));

    // Neither resume nor a replay of the key sends the unknown item again.
    for _ in 0..2 {
        let creation = fixture
            .harness
            .resume_batch(&batch.record.id)
            .await
            .unwrap();
        assert!(creation.driver.is_none());
    }
    let request: BatchRequest = serde_json::from_value(batch.record.request.clone()).unwrap();
    let replay = fixture
        .harness
        .create_batch("ambiguous".into(), request)
        .await
        .unwrap();
    assert!(replay.replayed && replay.driver.is_none());
    assert_eq!(fixture.submitted_calls(), 3);

    let unknown = batch.items[1].run_id.clone().unwrap();
    let reconciled = fixture.harness.reconcile(&unknown).await.unwrap();
    assert_eq!(reconciled.run.status, RunStatus::Queued);
    let batch = fixture.harness.get_batch(&batch.record.id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Submitted);
    assert_eq!(batch.items[1].run_status, Some(RunStatus::Queued));
    assert_eq!(fixture.submitted_calls(), 3);
}

#[tokio::test]
async fn the_same_key_replays_and_another_body_conflicts() {
    let fixture = Fixture::new();
    let request = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [4, 5]}}));
    let first = fixture.run_batch("replay", request.clone()).await;
    let replay = fixture
        .harness
        .create_batch("replay".into(), request.clone())
        .await
        .unwrap();
    assert!(replay.replayed && replay.driver.is_none());
    assert_eq!(replay.batch.record.id, first.record.id);
    assert_eq!(fixture.submitted_calls(), 2);

    let different = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [4, 6]}}));
    assert!(matches!(
        fixture
            .harness
            .create_batch("replay".into(), different)
            .await,
        Err(HarnessError::IdempotencyConflict)
    ));
    // A number spelt differently is a different request, never a replay.
    let spelt = fixture.request(
        json!({"seed": 1, "cfg": 2.0}),
        json!({"grid": {"steps": [4, 5]}}),
    );
    let integer = fixture.request(
        json!({"seed": 1, "cfg": 2}),
        json!({"grid": {"steps": [4, 5]}}),
    );
    fixture.run_batch("spelling", spelt).await;
    assert!(matches!(
        fixture
            .harness
            .create_batch("spelling".into(), integer)
            .await,
        Err(HarnessError::IdempotencyConflict)
    ));
}

/// Starts a four-item batch and holds item 1's prompt in flight, then lets
/// `interrupt` act before releasing it. Returns once the driver has ended.
async fn interrupted_batch(
    fixture: &Fixture,
    key: &str,
    interrupt: impl FnOnce(&CancellationToken, &CancellationToken),
) -> (String, BatchRequest) {
    let request = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2, 3, 4]}}));
    let creation = fixture
        .harness
        .create_batch(key.into(), request.clone())
        .await
        .unwrap();
    let id = creation.batch.record.id.clone();
    assert_eq!(creation.batch.state, BatchSummary::Submitting);
    let (stop, deadline) = (CancellationToken::new(), CancellationToken::new());
    let (entered, mut waiting) = mpsc::unbounded_channel();
    let gate = Arc::new(Gate {
        entered,
        release: Semaphore::new(0),
    });
    fixture.script.lock().unwrap().submit_gate = Some((2, gate.clone()));
    let driver = tokio::spawn(creation.driver.unwrap().run(
        stop.clone(),
        deadline.clone(),
        Fixture::observer(),
    ));
    timeout(Duration::from_secs(5), waiting.recv())
        .await
        .unwrap()
        .unwrap();
    interrupt(&stop, &deadline);
    gate.release.add_permits(1);
    timeout(Duration::from_secs(5), driver)
        .await
        .unwrap()
        .unwrap();
    (id, request)
}

async fn resume_by_replay(fixture: &Fixture, key: &str, request: BatchRequest) {
    let replay = fixture
        .harness
        .create_batch(key.into(), request)
        .await
        .unwrap();
    assert!(replay.replayed);
    replay
        .driver
        .expect("unsent items resume on a replay")
        .run(
            CancellationToken::new(),
            CancellationToken::new(),
            Fixture::observer(),
        )
        .await;
}

fn sent_steps(fixture: &Fixture) -> Vec<Value> {
    fixture
        .script
        .lock()
        .unwrap()
        .prompts
        .iter()
        .map(|(prompt, _)| prompt["3"]["inputs"]["steps"].clone())
        .collect()
}

#[tokio::test]
async fn a_graceful_stop_finishes_the_item_in_flight_and_resume_sends_only_the_rest() {
    let fixture = Fixture::new();
    let (id, request) = interrupted_batch(&fixture, "stop", |stop, _| stop.cancel()).await;
    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Interrupted);
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::Submitted,
            ItemSubmission::NotSubmitted,
            ItemSubmission::NotSubmitted
        ]
    );
    assert!(batch.items[2]
        .error
        .as_deref()
        .unwrap()
        .contains("shutting down"));
    assert_eq!(fixture.submitted_calls(), 2);

    // Re-posting the key resumes: each item key still maps to exactly one run.
    resume_by_replay(&fixture, "stop", request).await;
    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Submitted);
    assert_eq!(fixture.submitted_calls(), 4);
    let runs = fixture.harness.repository.list(10).unwrap();
    let mut indices: Vec<u32> = runs.iter().filter_map(|run| run.batch_index).collect();
    indices.sort_unstable();
    assert_eq!(indices, [0, 1, 2, 3]);
    assert_eq!(
        sent_steps(&fixture),
        [json!(1), json!(2), json!(3), json!(4)]
    );
}

#[tokio::test]
async fn an_expired_deadline_leaves_the_item_in_flight_unknown_and_never_resends_it() {
    let fixture = Fixture::new();
    let (id, request) =
        interrupted_batch(&fixture, "deadline", |_, deadline| deadline.cancel()).await;
    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Interrupted);
    // The in-flight item may have reached ComfyUI, so it is unknown; the rest were never sent.
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::OutcomeUnknown,
            ItemSubmission::NotSubmitted,
            ItemSubmission::NotSubmitted
        ]
    );
    assert!(batch.items[1]
        .error
        .as_deref()
        .unwrap()
        .contains("Shutdown deadline"));
    assert_eq!(fixture.submitted_calls(), 2);

    resume_by_replay(&fixture, "deadline", request).await;
    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Partial);
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::OutcomeUnknown,
            ItemSubmission::Submitted,
            ItemSubmission::Submitted
        ]
    );
    assert_eq!(fixture.submitted_calls(), 4);
    assert_eq!(
        sent_steps(&fixture),
        [json!(1), json!(2), json!(3), json!(4)]
    );
}

#[tokio::test]
async fn long_shared_values_cannot_multiply_past_the_parameter_budget() {
    let fixture = Fixture::new();
    let request = fixture.request(
        json!({"seed": 1, "prefix": "x".repeat(1_500_000)}),
        json!({"grid": {"steps": [1, 2, 3]}}),
    );
    let error = fixture.harness.preview_batch(&request).await.unwrap_err();
    assert!(
        matches!(&error, HarnessError::BatchTooLarge(message) if message.contains("bytes of parameters")),
        "{error}"
    );
    let small = fixture.request(
        json!({"seed": 1, "prefix": "x".repeat(1_000_000)}),
        json!({"grid": {"steps": [1, 2, 3]}}),
    );
    assert_eq!(
        fixture
            .harness
            .preview_batch(&small)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
}

#[tokio::test]
async fn restart_marks_unsent_items_interrupted_and_never_resumes_them() {
    let fixture = Fixture::new();
    let request = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2]}}));
    let creation = fixture
        .harness
        .create_batch("restart".into(), request)
        .await
        .unwrap();
    let id = creation.batch.record.id.clone();
    // The process dies before its driver submits anything: the claim is released.
    drop(creation.driver);

    // Another server instance over the same database starts up.
    let restarted = Harness::new(
        fixture.harness.config().clone(),
        fixture.harness.backend.clone(),
    )
    .unwrap();
    assert_eq!(restarted.interrupt_unfinished_batches().await.unwrap(), 1);
    assert_eq!(restarted.interrupt_unfinished_batches().await.unwrap(), 0);
    let batch = restarted.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Interrupted);
    assert_eq!(submissions(&batch), [ItemSubmission::NotSubmitted; 2]);
    assert!(batch.items[0]
        .error
        .as_deref()
        .unwrap()
        .contains("Resume the batch"));
    assert_eq!(fixture.submitted_calls(), 0, "recovery never submits");

    let resumed = restarted.resume_batch(&id).await.unwrap();
    resumed
        .driver
        .unwrap()
        .run(
            CancellationToken::new(),
            CancellationToken::new(),
            Fixture::observer(),
        )
        .await;
    assert_eq!(
        restarted.get_batch(&id).await.unwrap().state,
        BatchSummary::Submitted
    );
    assert_eq!(fixture.submitted_calls(), 2);
}

#[tokio::test]
async fn restart_leaves_a_batch_whose_driver_is_still_alive() {
    let fixture = Fixture::new();
    let request = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2]}}));
    let creation = fixture
        .harness
        .create_batch("alive".into(), request)
        .await
        .unwrap();
    let other = Harness::new(
        fixture.harness.config().clone(),
        fixture.harness.backend.clone(),
    )
    .unwrap();
    assert_eq!(other.interrupt_unfinished_batches().await.unwrap(), 0);
    assert_eq!(
        other
            .get_batch(&creation.batch.record.id)
            .await
            .unwrap()
            .state,
        BatchSummary::Submitting
    );
    // A second resume cannot start a competing driver.
    assert!(other
        .resume_batch(&creation.batch.record.id)
        .await
        .unwrap()
        .driver
        .is_none());
}

#[tokio::test]
async fn a_workflow_revision_change_mid_batch_leaves_the_rest_not_submitted() {
    let fixture = Fixture::new();
    let request = fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2, 3]}}));
    let creation = fixture
        .harness
        .create_batch("drift".into(), request)
        .await
        .unwrap();
    let id = creation.batch.record.id.clone();
    let (entered, mut waiting) = mpsc::unbounded_channel();
    let gate = Arc::new(Gate {
        entered,
        release: Semaphore::new(0),
    });
    fixture.script.lock().unwrap().submit_gate = Some((1, gate.clone()));
    let driver = tokio::spawn(creation.driver.unwrap().run(
        CancellationToken::new(),
        CancellationToken::new(),
        Fixture::observer(),
    ));
    timeout(Duration::from_secs(5), waiting.recv())
        .await
        .unwrap()
        .unwrap();
    let graph = fixture.harness.config().workflows.join("workflow.api.json");
    let mut changed: Value = serde_json::from_slice(&fs::read(&graph).unwrap()).unwrap();
    changed["9"]["inputs"]["filename_prefix"] = json!("changed");
    fs::write(&graph, serde_json::to_vec(&changed).unwrap()).unwrap();
    gate.release.add_permits(1);
    timeout(Duration::from_secs(5), driver)
        .await
        .unwrap()
        .unwrap();

    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(batch.state, BatchSummary::Interrupted);
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::NotSubmitted,
            ItemSubmission::NotSubmitted
        ]
    );
    assert!(batch.items[1]
        .error
        .as_deref()
        .unwrap()
        .contains("workflow changed"));
    assert_eq!(fixture.submitted_calls(), 1);
    // The pinned revision no longer exists, so resuming cannot submit the rest either.
    let resumed = fixture.harness.resume_batch(&id).await.unwrap();
    resumed
        .driver
        .unwrap()
        .run(
            CancellationToken::new(),
            CancellationToken::new(),
            Fixture::observer(),
        )
        .await;
    let batch = fixture.harness.get_batch(&id).await.unwrap();
    assert_eq!(submissions(&batch)[1..], [ItemSubmission::NotSubmitted; 2]);
    assert_eq!(fixture.submitted_calls(), 1);
}

#[tokio::test]
async fn a_panicking_driver_never_leaves_items_pending_or_successful() {
    let fixture = Fixture::new();
    fixture.script.lock().unwrap().panic_on_submit = Some(2);
    let batch = fixture
        .run_batch(
            "panic",
            fixture.request(json!({"seed": 1}), json!({"grid": {"steps": [1, 2, 3]}})),
        )
        .await;
    assert_eq!(batch.state, BatchSummary::Interrupted);
    // The panicking item has a run whose owner died: its outcome is unknown.
    assert_eq!(
        submissions(&batch),
        [
            ItemSubmission::Submitted,
            ItemSubmission::OutcomeUnknown,
            ItemSubmission::NotSubmitted
        ]
    );
    assert!(batch.items[2]
        .error
        .as_deref()
        .unwrap()
        .contains("stopped unexpectedly"));
    assert_eq!(fixture.submitted_calls(), 2);
}

#[tokio::test]
async fn pinned_requests_reject_unknown_and_non_seed_parameters_before_any_write() {
    let fixture = Fixture::new();
    let not_seed = fixture.request(
        json!({}),
        json!({"seeds": {"parameter": "steps", "values": [1]}}),
    );
    let unknown = fixture.request(json!({"steps": 5}), json!({"grid": {"nope": [1]}}));
    let stale = BatchRequest {
        workflow_revision: "stale".into(),
        ..fixture.request(json!({"steps": 5}), seeds(json!([1])))
    };
    for (request, expected) in [(not_seed, "not a seed"), (unknown, "not defined")] {
        let error = fixture.harness.preview_batch(&request).await.unwrap_err();
        assert!(
            matches!(&error, HarnessError::InvalidBatchRequest(message) if message.contains(expected)),
            "{error}"
        );
    }
    assert!(matches!(
        fixture.harness.preview_batch(&stale).await,
        Err(HarnessError::WorkflowChanged)
    ));
    assert_eq!(fixture.submitted_calls(), 0);
    assert_eq!(
        fixture.script.lock().unwrap().object_info_calls,
        0,
        "previews never touch the backend"
    );
}

#[tokio::test]
async fn previews_show_effective_parameters_and_inherited_inputs_per_item() {
    let fixture = Fixture::new();
    let request = BatchRequest {
        preset: Some("fast".into()),
        ..fixture.request(json!({}), json!({"grid": {"seed": [5, 6]}}))
    };
    let plan = fixture.harness.preview_batch(&request).await.unwrap();
    assert_eq!(plan.items.len(), 2);
    let first = &plan.items[0];
    assert_eq!(
        first.parameters,
        BTreeMap::from([("seed".to_owned(), json!(5))])
    );
    assert_eq!(first.sweep_values, first.parameters);
    // The preset supplies steps; cfg is untouched and inherited from the graph.
    assert_eq!(
        first.effective_parameters,
        BTreeMap::from([
            ("seed".to_owned(), json!(5)),
            ("steps".to_owned(), json!(10))
        ])
    );
    let inherited: Vec<(&str, &Value)> = first
        .inherited_inputs
        .iter()
        .map(|input| (input.parameter.as_str(), &input.value))
        .collect();
    assert_eq!(
        inherited,
        [("cfg", &json!(7)), ("prefix", &json!("genaimage"))]
    );
}
