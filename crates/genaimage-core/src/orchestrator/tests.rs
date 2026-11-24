use std::sync::Mutex;

use tempfile::TempDir;
use tokio::{
    sync::{mpsc, Semaphore},
    time::timeout,
};

use super::*;
use crate::{
    async_trait,
    image_backend::{
        BackendEvent, EventMonitor, HistorySnapshot, OutputReference, PromptResponse,
        QueueSnapshot, UploadResponse,
    },
    ExtensionRegistry, HarnessConfig,
};

const IMAGE: &[u8] = b"fake image output";

#[derive(Default)]
struct FakeImageBackend {
    state: Arc<Mutex<FakeState>>,
    endpoint: String,
}

#[derive(Default)]
struct FakeState {
    lose_submit_response: bool,
    submission_warnings: bool,
    submission: Option<(Value, String, String)>,
    queue_position: Option<QueuePosition>,
    history_outcome: HistoryOutcome,
    delete_requested: bool,
    interrupt_requested: bool,
    downloads: usize,
    submit_gate: Option<Arc<Gate>>,
    object_info_gate: Option<Arc<Gate>>,
    uploaded_inputs: Vec<(PathBuf, Vec<u8>)>,
    upload_gate: Option<Arc<Gate>>,
    queue_gate: Option<Arc<Gate>>,
    history_gate: Option<Arc<Gate>>,
    download_gate: Option<Arc<Gate>>,
    queue_once_gate: Option<Arc<Gate>>,
    interrupt_gate: Option<Arc<Gate>>,
    fail_interrupt: bool,
}

struct Gate {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}

impl Gate {
    async fn wait(&self) {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
    }
}

fn new_gate() -> (Arc<Gate>, mpsc::UnboundedReceiver<()>) {
    let (entered, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(Gate {
            entered,
            release: Semaphore::new(0),
        }),
        receiver,
    )
}

#[derive(Clone, Copy)]
enum QueuePosition {
    Pending,
    Running,
}

#[async_trait]
impl ImageBackend for FakeImageBackend {
    fn name(&self) -> &str {
        "fake backend"
    }

    fn base_url(&self) -> &str {
        if self.endpoint.is_empty() {
            "fake://local"
        } else {
            &self.endpoint
        }
    }

    async fn health(&self) -> Result<BackendHealth> {
        Ok(BackendHealth {
            system: json!({"ready": true}),
        })
    }

    async fn object_info(&self) -> Result<Value> {
        let gate = self.state.lock().unwrap().object_info_gate.take();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        Ok(json!({"KSampler": {}, "SaveImage": {}, "LoadImage": {}, "LoadImageMask": {}}))
    }

    async fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> Result<PromptResponse> {
        let (gate, response) = {
            let mut state = self.state.lock().unwrap();
            assert!(
                state.submission.is_none(),
                "must not submit the same run twice"
            );
            state.submission = Some((workflow.clone(), client_id.to_owned(), prompt_id.to_owned()));
            if state.lose_submit_response {
                return Err(HarnessError::Backend {
                    backend: self.name().to_owned(),
                    message: "submit response lost".to_owned(),
                });
            }
            state.queue_position = Some(QueuePosition::Pending);
            let response = PromptResponse {
                prompt_id: prompt_id.to_owned(),
                number: None,
                node_errors: if state.submission_warnings {
                    json!({"node": "invalid"})
                } else {
                    Value::Null
                },
            };
            (state.submit_gate.clone(), response)
        };
        if let Some(gate) = gate {
            gate.wait().await;
        }
        Ok(response)
    }

    async fn queue(&self) -> Result<QueueSnapshot> {
        let gate = {
            let mut state = self.state.lock().unwrap();
            state
                .queue_once_gate
                .take()
                .or_else(|| state.queue_gate.clone())
        };
        if let Some(gate) = gate {
            gate.wait().await;
        }
        let state = self.state.lock().unwrap();
        let mut snapshot = QueueSnapshot {
            running: vec![],
            pending: vec![],
            raw: Value::Null,
        };
        if let Some(position) = state.queue_position {
            let id = state.submission.as_ref().unwrap().2.clone();
            match position {
                QueuePosition::Pending => snapshot.pending.push(id),
                QueuePosition::Running => snapshot.running.push(id),
            }
        }
        Ok(snapshot)
    }

    async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot> {
        let gate = self.state.lock().unwrap().history_gate.clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        let state = self.state.lock().unwrap();
        assert_eq!(prompt_id, state.submission.as_ref().unwrap().2);
        let success = state.history_outcome == HistoryOutcome::Completed;
        Ok(HistorySnapshot {
            prompt_id: prompt_id.to_owned(),
            outcome: state.history_outcome,
            outputs: if success {
                vec![OutputReference {
                    node_id: "9".to_owned(),
                    kind: "images".to_owned(),
                    filename: "result.png".to_owned(),
                    subfolder: String::new(),
                    output_type: "output".to_owned(),
                }]
            } else {
                vec![]
            },
            raw: json!({"id": prompt_id, "outcome": state.history_outcome}),
        })
    }

    async fn delete_queued(&self, prompt_id: &str) -> Result<Value> {
        let mut state = self.state.lock().unwrap();
        assert_eq!(prompt_id, state.submission.as_ref().unwrap().2);
        assert!(matches!(state.queue_position, Some(QueuePosition::Pending)));
        state.delete_requested = true;
        Ok(Value::Null)
    }

    async fn interrupt(&self, prompt_id: &str) -> Result<Value> {
        let gate = self.state.lock().unwrap().interrupt_gate.clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        let mut state = self.state.lock().unwrap();
        assert_eq!(prompt_id, state.submission.as_ref().unwrap().2);
        assert!(matches!(state.queue_position, Some(QueuePosition::Running)));
        state.interrupt_requested = true;
        if state.fail_interrupt {
            return Err(HarnessError::CancellationRejected(
                "test interrupt rejection".to_owned(),
            ));
        }
        Ok(Value::Null)
    }

    async fn upload_image(&self, path: &Path) -> Result<UploadResponse> {
        let gate = self.state.lock().unwrap().upload_gate.take();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        let bytes = fs::read(path)?;
        self.state
            .lock()
            .unwrap()
            .uploaded_inputs
            .push((path.to_owned(), bytes));
        Ok(UploadResponse {
            name: path.file_name().unwrap().to_str().unwrap().to_owned(),
            subfolder: String::new(),
            kind: "input".into(),
        })
    }

    async fn download_output(&self, output: &OutputReference) -> Result<Vec<u8>> {
        let gate = self.state.lock().unwrap().download_gate.clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        assert_eq!(output.filename, "result.png");
        self.state.lock().unwrap().downloads += 1;
        Ok(IMAGE.to_vec())
    }

    async fn monitor_events(&self, client_id: &str) -> Result<EventMonitor> {
        let state = self.state.lock().unwrap();
        let (_, submitted_client, prompt_id) = state.submission.as_ref().unwrap();
        assert_eq!(client_id, submitted_client);
        let (sender, receiver) = mpsc::channel(2);
        for id in ["unrelated-prompt", prompt_id] {
            sender
                .try_send(BackendEvent {
                    event_type: "progress".to_owned(),
                    prompt_id: Some(id.to_owned()),
                    node_id: Some("3".to_owned()),
                    value: Some(4),
                    maximum: Some(10),
                    message: None,
                    raw: Value::Null,
                })
                .unwrap();
        }
        Ok(EventMonitor::new(
            receiver,
            tokio::spawn(async { drop(sender) }),
        ))
    }
}

fn test_config(workspace: &Path) -> crate::ResolvedConfig {
    HarnessConfig::load_with(workspace, None, workspace, |key| match key {
        "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => Some(workspace.display().to_string()),
        _ => None,
    })
    .unwrap()
}

fn setup(lose_submit_response: bool) -> (TempDir, Harness, Arc<Mutex<FakeState>>, RunRequest) {
    let workspace = tempfile::tempdir().unwrap();
    let mut config = test_config(workspace.path());
    config.state_database = workspace.path().join("state.sqlite3");
    config.runs = workspace.path().join("runs");
    fs::create_dir_all(&config.workflows).unwrap();
    fs::write(
        config.workflows.join("workflow.api.json"),
        include_str!("../../tests/fixtures/contract-workflow.api.json"),
    )
    .unwrap();
    fs::write(
        config.workflows.join("contract.genaimage.json"),
        include_str!("../../tests/fixtures/contract-workflow.genaimage.json"),
    )
    .unwrap();
    let state = Arc::new(Mutex::new(FakeState {
        lose_submit_response,
        ..FakeState::default()
    }));
    let registry = ExtensionRegistry::from_config(&config, |_| {
        Ok(FakeImageBackend {
            state: Arc::clone(&state),
            ..Default::default()
        })
    })
    .unwrap();
    assert!(registry.image_backend("local").is_none());
    let backend = registry
        .image_backend(crate::extensions::COMFYUI_BACKEND)
        .unwrap();
    let harness = Harness::new(config, backend).unwrap();
    let request = RunRequest {
        workflow: "contract-workflow".to_owned(),
        preset: Some("fast".to_owned()),
        parameters: BTreeMap::from([("seed".to_owned(), json!(123))]),
        version: None,
    };
    (workspace, harness, state, request)
}

#[tokio::test]
async fn path_only_backend_uploads_verified_snapshots_for_submit_and_rerun() {
    let (workspace, harness, state, mut request) = setup(true);
    let root = &harness.config.workflows;
    let graph_path = root.join("workflow.api.json");
    let manifest_path = root.join("contract.genaimage.json");
    let mut graph: Value = serde_json::from_slice(&fs::read(&graph_path).unwrap()).unwrap();
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let references = [
        ("reference", "12", "LoadImage", b"original".as_slice()),
        ("reference_mask", "13", "LoadImageMask", b"mask".as_slice()),
    ];
    for (parameter, node, class, bytes) in references {
        graph[node] = json!({"class_type": class, "inputs": {"image": "default.png"}});
        manifest["parameters"][parameter] =
            json!({"node":node, "input":"image", "kind":"image_upload", "required":true});
        let filename = format!("{parameter}.png");
        fs::write(workspace.path().join(&filename), bytes).unwrap();
        request.parameters.insert(parameter.into(), json!(filename));
    }
    fs::write(&graph_path, serde_json::to_vec(&graph).unwrap()).unwrap();
    let original_manifest = serde_json::to_vec(&manifest).unwrap();
    fs::write(&manifest_path, &original_manifest).unwrap();
    let (gate, mut entered) = new_gate();
    state.lock().unwrap().upload_gate = Some(gate.clone());
    let submit = {
        let harness = harness.clone();
        tokio::spawn(async move { harness.submit(request).await.unwrap() })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let first_reference = workspace.path().join("reference.png");
    fs::write(&first_reference, b"modified").unwrap();
    gate.release.add_permits(1);
    let source = submit.await.unwrap();
    fs::write(&first_reference, b"original").unwrap();
    assert_eq!(source.status, RunStatus::UnknownSubmission);
    let before = serde_json::to_value(harness.repository.get(&source.id).unwrap()).unwrap();
    let source_preflight = Path::new(&source.output_dir).join("preflight.json");
    let preflight_before = fs::read(&source_preflight).unwrap();
    let preflight: Value = serde_json::from_slice(&preflight_before).unwrap();
    assert_eq!(
        preflight["source_inputs"][0]["sha256"],
        hex::encode(Sha256::digest(b"original"))
    );

    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.object_info_gate = Some(gate.clone());
        state.submission = None;
        state.lose_submit_response = false;
    }
    let rerun = {
        let harness = harness.clone();
        let id = source.id.clone();
        tokio::spawn(async move { harness.rerun("rerun".into(), id).await.unwrap() })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    for (parameter, ..) in references {
        fs::write(
            workspace.path().join(format!("{parameter}.png")),
            b"modified",
        )
        .unwrap();
    }
    fs::write(&graph_path, b"{}").unwrap();
    fs::write(&manifest_path, b"{}").unwrap();
    gate.release.add_permits(1);
    let result = rerun.await.unwrap();
    let run = result.run;
    assert!(!result.replayed);
    assert_ne!(run.id, source.id);
    assert_ne!(run.prompt_id, source.prompt_id);
    assert_eq!(run.rerun_of.as_deref(), Some(source.id.as_str()));
    assert_eq!(run.parameters, source.parameters);
    assert_eq!(run.workflow_version, source.workflow_version);
    assert_eq!(run.workflow_sha256, source.workflow_sha256);
    assert_eq!(
        run.workflow_manifest_sha256,
        source.workflow_manifest_sha256
    );
    {
        let state = state.lock().unwrap();
        let expected = references.iter().cycle().take(4);
        assert_eq!(state.uploaded_inputs.len(), 4);
        for ((path, actual), (parameter, _, _, bytes)) in state.uploaded_inputs.iter().zip(expected)
        {
            let filename = format!("{parameter}.png");
            assert_eq!(path.file_name().unwrap(), filename.as_str());
            assert_ne!(*path, workspace.path().join(filename));
            assert_eq!(actual.as_slice(), *bytes);
            assert!(
                !path.exists(),
                "snapshot should be removed: {}",
                path.display()
            );
        }
        let prompt = &state.submission.as_ref().unwrap().0;
        assert_eq!(prompt["3"]["inputs"], json!({"seed": 123, "steps": 10}));
    }
    assert_eq!(
        fs::read(Path::new(&run.output_dir).join("workflow-manifest.json")).unwrap(),
        original_manifest
    );
    assert_eq!(
        serde_json::to_value(harness.repository.get(&source.id).unwrap()).unwrap(),
        before
    );
    assert_eq!(fs::read(source_preflight).unwrap(), preflight_before);
    let replay = harness.rerun("rerun".into(), source.id).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.run.id, run.id);
}

#[test]
fn discovery_reports_reserved_dot_segment_ids() {
    let (_workspace, harness, _, _) = setup(false);
    let path = harness.config().workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for id in [".", ".."] {
        manifest["id"] = json!(id);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let discovery = harness.discover_workflows().unwrap();
        assert!(discovery.manifests.is_empty());
        assert!(discovery.indexable_manifest_paths.is_empty());
        assert_eq!(discovery.diagnostics.len(), 1);
        let message = discovery.diagnostics[0].error.to_string();
        assert!(message.contains("reserved URL path segment"));
        assert!(message.contains("choose a different ID"));
    }
}

#[tokio::test(start_paused = true)]
async fn runs_to_completion_with_progress_and_checksummed_output() {
    let (_workspace, harness, state, request) = setup(false);
    assert_eq!(harness.health().await.unwrap().system["ready"], true);
    let run = harness.submit(request).await.unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    {
        let mut state = state.lock().unwrap();
        let (submitted, client_id, prompt_id) = state.submission.as_ref().unwrap();
        assert_eq!(submitted["3"]["inputs"], json!({"seed": 123, "steps": 10}));
        assert_eq!(submitted["9"]["inputs"]["filename_prefix"], "genaimage");
        assert_eq!(client_id, &run.client_id);
        assert_eq!(prompt_id, &run.prompt_id);
        state.queue_position = Some(QueuePosition::Running);
    }
    let mut receiver = harness.events().subscribe();
    let waiter = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.wait_for_run(&id).await.unwrap() })
    };
    timeout(Duration::from_secs(3), async {
        let mut progress_seen = false;
        let mut running_seen = false;
        while !progress_seen || !running_seen {
            let update = receiver.recv().await.unwrap();
            if let Event::RunProgress {
                run: updated_run,
                backend_event,
            } = update.event
            {
                if let Some(event) = backend_event {
                    assert_eq!(event.prompt_id.as_deref(), Some(run.prompt_id.as_str()));
                    assert_eq!(event.value, Some(4));
                    assert!(!progress_seen, "only one event belongs to this run");
                    progress_seen = true;
                }
                running_seen |= updated_run.status == RunStatus::Running;
            }
        }
    })
    .await
    .unwrap();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_outcome = HistoryOutcome::Completed;
    }
    let completed = timeout(Duration::from_secs(3), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, RunStatus::Completed);
    let mut replay = harness.events().subscribe_since(0).unwrap();
    assert!(matches!(
        replay.recv().await.unwrap().event,
        Event::BackendHealthChanged { .. }
    ));
    assert!(matches!(
        replay.recv().await.unwrap().event,
        Event::RunQueued { .. }
    ));
    let mut started = false;
    let mut output_seen = false;
    loop {
        match replay.recv().await.unwrap().event {
            Event::RunStarted { run: started_run } => {
                assert_eq!(started_run.id, run.id);
                started = true;
            }
            Event::RunOutput { run_id, output } => {
                assert_eq!(run_id, run.id);
                assert_eq!(output.run_id, run.id);
                output_seen = true;
            }
            Event::RunCompleted { run: finished } => {
                assert_eq!(finished.status, RunStatus::Completed);
                break;
            }
            _ => {}
        }
    }
    assert!(started && output_seen);
    let result = harness.reconcile(&run.id).await.unwrap();
    let [output] = result.outputs.as_slice() else {
        panic!("expected one output")
    };
    assert_eq!(fs::read(&output.local_path).unwrap(), IMAGE);
    assert_eq!(
        output.sha256,
        artifacts::checksum_file(Path::new(&output.local_path))
            .unwrap()
            .sha256
    );
    let manifest: artifacts::RunManifest =
        serde_json::from_slice(&fs::read(completed.manifest_path.unwrap()).unwrap()).unwrap();
    assert_eq!(manifest.parameters["seed"], 123);
    assert_eq!(manifest.comfyui_base_url, "fake://local");
    assert_eq!(manifest.outputs[0].sha256, output.sha256);
    assert_eq!(state.lock().unwrap().downloads, 1);
    let events = harness.run_events(&run.id, 20).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "progress")
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_history_never_materialises_outputs() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_outcome = HistoryOutcome::Failed;
    }
    let result = harness.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Failed);
    assert!(result.run.manifest_path.is_none());
    assert!(result.outputs.is_empty());
    assert_eq!(state.lock().unwrap().downloads, 0);
    let mut replay = harness.events().subscribe_since(0).unwrap();
    assert!(matches!(
        replay.recv().await.unwrap().event,
        Event::RunQueued { .. }
    ));
    assert!(
        matches!(replay.recv().await.unwrap().event, Event::RunFailed { run } if run.status == RunStatus::Failed)
    );
}

#[tokio::test]
async fn cancellation_waits_for_backend_evidence_for_pending_and_running_prompts() {
    for position in [QueuePosition::Pending, QueuePosition::Running] {
        let (_workspace, harness, state, request) = setup(false);
        let run = harness.submit(request).await.unwrap();
        state.lock().unwrap().queue_position = Some(position);
        assert_eq!(
            harness.cancel(&run.id).await.unwrap().status,
            RunStatus::Cancelling
        );
        assert_eq!(
            harness.reconcile(&run.id).await.unwrap().run.status,
            RunStatus::Cancelling
        );
        {
            let mut state = state.lock().unwrap();
            assert_eq!(
                state.delete_requested,
                matches!(position, QueuePosition::Pending)
            );
            assert_eq!(
                state.interrupt_requested,
                matches!(position, QueuePosition::Running)
            );
            state.queue_position = None;
            state.history_outcome = HistoryOutcome::Cancelled;
        }
        let result = harness.reconcile(&run.id).await.unwrap();
        assert_eq!(result.run.status, RunStatus::Cancelled);
        assert!(result.outputs.is_empty());
        let mut replay = harness.events().subscribe_since(0).unwrap();
        assert!(matches!(
            replay.recv().await.unwrap().event,
            Event::RunQueued { .. }
        ));
        assert!(
            matches!(replay.recv().await.unwrap().event, Event::RunCancelled { run } if run.status == RunStatus::Cancelled)
        );
    }
}

#[tokio::test]
async fn ordinary_execution_failure_after_cancel_is_failed_not_cancelled() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    state.lock().unwrap().queue_position = Some(QueuePosition::Running);
    assert_eq!(
        harness.cancel(&run.id).await.unwrap().status,
        RunStatus::Cancelling
    );
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_outcome = HistoryOutcome::Failed;
    }
    let result = harness.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Failed);
    assert!(result.outputs.is_empty());
}

#[tokio::test]
async fn queue_and_history_gap_after_cancel_leaves_run_open_for_later_success() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    state.lock().unwrap().queue_position = Some(QueuePosition::Running);
    assert_eq!(
        harness.cancel(&run.id).await.unwrap().status,
        RunStatus::Cancelling
    );
    state.lock().unwrap().queue_position = None;
    let result = harness.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Cancelling);
    assert!(result.outputs.is_empty());

    state.lock().unwrap().history_outcome = HistoryOutcome::Completed;
    let result = harness.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Completed);
    assert_eq!(fs::read(&result.outputs[0].local_path).unwrap(), IMAGE);
}

#[tokio::test]
async fn settled_not_found_still_allows_late_adoption_from_queue_or_history() {
    for seen_in_queue in [false, true] {
        let (_workspace, harness, state, request) = setup(true);
        let run = harness.submit(request).await.unwrap();
        assert_eq!(run.status, RunStatus::UnknownSubmission);
        let mut replay = harness.events().subscribe_since(0).unwrap();
        assert!(
            matches!(replay.recv().await.unwrap().event, Event::RunUnknownSubmission { run } if run.status == RunStatus::UnknownSubmission)
        );
        state.lock().unwrap().history_outcome = HistoryOutcome::NotFound;
        let settling = harness.reconcile(&run.id).await.unwrap();
        assert_eq!(settling.outcome, ReconcileOutcome::StillAmbiguous);
        assert!(settling.settles_at.unwrap() > Utc::now());
        let db = rusqlite::Connection::open(&harness.config().state_database).unwrap();
        db.execute("UPDATE run_events SET created_at = '2000-01-01T00:00:00Z' WHERE run_id = ?1 AND event_type = 'submission_attempted'", [&run.id]).unwrap();
        for (history, expected) in [
            (HistoryOutcome::Unresolved, ReconcileOutcome::StillAmbiguous),
            (HistoryOutcome::NotFound, ReconcileOutcome::NotFound),
        ] {
            state.lock().unwrap().history_outcome = history;
            let result = harness.reconcile(&run.id).await.unwrap();
            assert_eq!(result.run.status, RunStatus::UnknownSubmission);
            assert_eq!(result.run.updated_at, run.updated_at);
            assert_eq!(result.outcome, expected);
            let events = harness.run_events(&run.id, 10).unwrap();
            assert_eq!(events.last().unwrap().event_type, "run_reconciled");
            assert_eq!(events.last().unwrap().payload, json!({"outcome": expected}));
        }
        // Admission can still happen after the window and a not-found result.
        if seen_in_queue {
            state.lock().unwrap().queue_position = Some(QueuePosition::Pending);
            let result = harness.reconcile(&run.id).await.unwrap();
            assert_eq!(result.run.status, RunStatus::Queued);
            assert_eq!(result.outcome, ReconcileOutcome::Active);
            assert_eq!(result.run.prompt_id, run.prompt_id);
            state.lock().unwrap().queue_position = None;
            let result = harness.reconcile(&run.id).await.unwrap();
            assert_eq!(result.run.status, RunStatus::Queued);
            assert_eq!(result.outcome, ReconcileOutcome::StillAmbiguous);
        }
        {
            let mut state = state.lock().unwrap();
            state.queue_position = None;
            state.history_outcome = HistoryOutcome::Completed;
        }
        let result = harness.reconcile(&run.id).await.unwrap();
        assert_eq!(result.run.status, RunStatus::Completed);
        assert_eq!(result.outcome, ReconcileOutcome::Terminal);
        assert_eq!(result.run.prompt_id, run.prompt_id);
        assert_eq!(result.outputs.len(), 1);
    }
}

#[test]
fn registry_does_not_replace_an_extension_or_fall_back_on_factory_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let config = test_config(workspace.path());
    let error = ExtensionRegistry::from_config::<FakeImageBackend>(&config, |profile| {
        assert_eq!(profile.base_url, config.comfyui.base_url);
        Err(HarnessError::Configuration(
            "factory refused the profile".to_owned(),
        ))
    });
    assert!(
        matches!(error, Err(HarnessError::Configuration(message)) if message == "factory refused the profile")
    );

    let mut registry =
        ExtensionRegistry::from_config(&config, |_| Ok(FakeImageBackend::default())).unwrap();
    let original = registry
        .image_backend(crate::extensions::COMFYUI_BACKEND)
        .unwrap();
    assert!(registry
        .register_image_backend(
            crate::extensions::COMFYUI_BACKEND,
            Arc::new(FakeImageBackend::default())
        )
        .is_err());
    assert!(Arc::ptr_eq(
        &original,
        &registry
            .image_backend(crate::extensions::COMFYUI_BACKEND)
            .unwrap()
    ));
}

#[tokio::test]
async fn shared_database_refuses_cross_workspace_reconciliation() {
    let (_workspace, harness, _state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    harness
        .repository
        .update_status(&run.id, RunStatus::Cancelling, None, None)
        .unwrap();
    let second = tempfile::tempdir().unwrap();
    let mut config = harness.config().clone();
    config.workspace = second.path().canonicalize().unwrap();
    config.claim_legacy_state = true;
    let async_error =
        match Harness::new_async(config.clone(), Arc::new(FakeImageBackend::default())).await {
            Ok(_) => panic!("a foreign workspace must not open the database asynchronously"),
            Err(error) => error.to_string(),
        };
    assert!(
        async_error.contains("separate state database"),
        "{async_error}"
    );
    let error = match Harness::new(config, Arc::new(FakeImageBackend::default())) {
        Ok(_) => panic!("a foreign workspace must not open the database"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("workspace") && error.contains("separate state database"),
        "{error}"
    );
    let wrong_endpoint = FakeImageBackend {
        endpoint: "fake://other".into(),
        ..Default::default()
    };
    let error = match Harness::new(harness.config().clone(), Arc::new(wrong_endpoint)) {
        Ok(_) => panic!("a foreign endpoint must not open the database"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("fake://local") && error.contains("fake://other"),
        "{error}"
    );
    assert_eq!(
        harness.get_run(&run.id).unwrap().status,
        RunStatus::Cancelling
    );
}

#[tokio::test]
async fn verified_legacy_state_reconciles_using_original_paths() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let mut config = harness.config().clone();
    let original_dir = run.output_dir.clone();
    drop(harness);
    let db = rusqlite::Connection::open(&config.state_database).unwrap();
    db.execute_batch("ALTER TABLE runs DROP COLUMN rerun_of; ALTER TABLE runs DROP COLUMN workflow_manifest_sha256; ALTER TABLE runs DROP COLUMN preflight_sha256; ALTER TABLE runs DROP COLUMN uses_submission_lock; ALTER TABLE runs DROP COLUMN submission_acknowledged; ALTER TABLE runs DROP COLUMN node_warnings_json; DROP TABLE run_submission_keys; DROP INDEX runs_created_id_idx; DROP TABLE genaimage_database_identity; DROP TABLE run_media; DROP INDEX outputs_created_id_idx; PRAGMA user_version=2;")
        .unwrap();
    drop(db);
    let make_backend = || {
        Arc::new(FakeImageBackend {
            state: Arc::clone(&state),
            ..Default::default()
        })
    };
    assert!(Harness::new(config.clone(), make_backend()).is_err());
    config.claim_legacy_state = true;
    let restored = Harness::new(config, make_backend()).unwrap();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_outcome = HistoryOutcome::Completed;
    }
    let result = restored.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Completed);
    assert_eq!(result.run.output_dir, original_dir);
    assert!(Path::new(&result.outputs[0].local_path).starts_with(original_dir));
    assert!(Path::new(result.run.manifest_path.as_ref().unwrap()).is_file());
}

#[tokio::test]
async fn workspaces_can_use_the_same_explicit_run_version() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut harnesses = Vec::new();
    for name in ["first", "second"] {
        let workspace = root.path().join(name);
        fs::create_dir_all(&workspace).unwrap();
        let config = HarnessConfig::load_with(&workspace, None, root.path(), |key| match key {
            "HOME" | "XDG_CONFIG_HOME" => Some(root.path().display().to_string()),
            "XDG_DATA_HOME" => Some(data.display().to_string()),
            _ => None,
        })
        .unwrap();
        fs::create_dir_all(&config.workflows).unwrap();
        fs::write(
            config.workflows.join("workflow.api.json"),
            include_str!("../../tests/fixtures/contract-workflow.api.json"),
        )
        .unwrap();
        fs::write(
            config.workflows.join("contract.genaimage.json"),
            include_str!("../../tests/fixtures/contract-workflow.genaimage.json"),
        )
        .unwrap();
        harnesses.push(Harness::new(config, Arc::new(FakeImageBackend::default())).unwrap());
    }
    let request = RunRequest {
        workflow: "contract-workflow".into(),
        preset: Some("fast".into()),
        parameters: BTreeMap::new(),
        version: Some("v1".into()),
    };
    let first = harnesses[0].submit(request.clone()).await.unwrap();
    let second = harnesses[1].submit(request).await.unwrap();
    assert_ne!(first.output_dir, second.output_dir);
    assert!(Path::new(&first.output_dir).is_dir() && Path::new(&second.output_dir).is_dir());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_reconciles_publish_each_transition_once() {
    for independent in [false, true] {
        let (_workspace, harness, state, request) = setup(false);
        let other = if independent {
            Harness::with_event_bus(
                harness.config().clone(),
                harness.backend.clone(),
                harness.events().clone(),
            )
            .unwrap()
        } else {
            harness.clone()
        };
        let run = harness.submit(request).await.unwrap();
        let mut events = harness.events().subscribe();
        let (gate, mut entered) = new_gate();
        {
            let mut state = state.lock().unwrap();
            state.queue_position = Some(QueuePosition::Running);
            state.queue_gate = Some(gate.clone());
        }
        let first = {
            let harness = harness.clone();
            let id = run.id.clone();
            tokio::spawn(async move { harness.reconcile(&id).await.unwrap() })
        };
        timeout(Duration::from_secs(2), entered.recv())
            .await
            .unwrap()
            .unwrap();
        let second = {
            let harness = other.clone();
            let id = run.id.clone();
            tokio::spawn(async move { harness.reconcile(&id).await.unwrap() })
        };
        assert!(
            timeout(Duration::from_millis(100), entered.recv())
                .await
                .is_err(),
            "second reconcile entered the backend concurrently"
        );
        gate.release.add_permits(2);
        first.await.unwrap();
        second.await.unwrap();
        assert!(matches!(
            events.recv().await.unwrap().event,
            Event::RunStarted { .. }
        ));
        assert!(timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err());

        {
            let mut state = state.lock().unwrap();
            state.queue_gate = None;
            state.queue_position = None;
            state.history_outcome = HistoryOutcome::Completed;
        }
        let (gate, mut entered) = new_gate();
        state.lock().unwrap().history_gate = Some(gate.clone());
        let first = {
            let harness = harness.clone();
            let id = run.id.clone();
            tokio::spawn(async move { harness.reconcile(&id).await.unwrap() })
        };
        timeout(Duration::from_secs(2), entered.recv())
            .await
            .unwrap()
            .unwrap();
        let second = {
            let harness = other.clone();
            let id = run.id.clone();
            tokio::spawn(async move { harness.reconcile(&id).await.unwrap() })
        };
        assert!(
            timeout(Duration::from_millis(100), entered.recv())
                .await
                .is_err(),
            "second reconcile entered history concurrently"
        );
        gate.release.add_permits(2);
        assert_eq!(first.await.unwrap().run.status, RunStatus::Completed);
        assert_eq!(second.await.unwrap().run.status, RunStatus::Completed);
        assert!(matches!(
            events.recv().await.unwrap().event,
            Event::RunOutput { .. }
        ));
        assert!(matches!(
            events.recv().await.unwrap().event,
            Event::RunCompleted { .. }
        ));
        assert!(timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err());
        assert_eq!(state.lock().unwrap().downloads, 1);
    }
}

#[tokio::test]
async fn failed_terminal_record_forces_live_and_replay_resync() {
    for (outcome, status, event_type) in [
        (
            HistoryOutcome::Completed,
            RunStatus::Completed,
            "run_completed",
        ),
        (HistoryOutcome::Failed, RunStatus::Failed, "run_failed"),
        (
            HistoryOutcome::Cancelled,
            RunStatus::Cancelled,
            "run_cancelled",
        ),
    ] {
        let (_workspace, harness, state, request) = setup(false);
        let run = harness.submit(request).await.unwrap();
        let mut events = harness.events().subscribe();
        let db = rusqlite::Connection::open(&harness.config().state_database).unwrap();
        db.execute_batch(&format!("CREATE TRIGGER reject_terminal BEFORE INSERT ON run_events WHEN NEW.event_type = '{event_type}' BEGIN SELECT RAISE(FAIL, 'test failure'); END;")).unwrap();
        {
            let mut state = state.lock().unwrap();
            state.queue_position = None;
            state.history_outcome = outcome;
        }
        assert!(harness.reconcile(&run.id).await.is_err());
        assert_eq!(harness.get_run(&run.id).unwrap().status, status);
        let invalidated = loop {
            match timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
            {
                Ok(_) => {}
                Err(crate::ReceiveError::ResyncRequired(crate::ResyncReason::Invalidated {
                    id,
                })) => break id,
                other => panic!("expected invalidation, got {other:?}"),
            }
        };
        assert_eq!(
            harness.events().subscribe_since(1).err(),
            Some(crate::ResyncReason::Invalidated { id: invalidated })
        );
        assert_eq!(events.recv().await.err(), Some(crate::ReceiveError::Closed));
        assert_eq!(harness.reconcile(&run.id).await.unwrap().run.status, status);
    }
}

#[tokio::test]
async fn acknowledged_submission_record_failure_invalidates_the_stream() {
    let (_workspace, harness, state, request) = setup(false);
    state.lock().unwrap().submission_warnings = true;
    let mut events = harness.events().subscribe();
    let db = rusqlite::Connection::open(&harness.config().state_database).unwrap();
    db.execute_batch("CREATE TRIGGER reject_acknowledgement BEFORE INSERT ON run_events WHEN NEW.event_type = 'prompt_queued' BEGIN SELECT RAISE(FAIL, 'test failure'); END;").unwrap();
    assert!(harness.submit(request).await.is_err());
    let runs = harness.list_runs(1).unwrap();
    assert_eq!(runs[0].status, RunStatus::Queued);
    assert!(matches!(
        events.recv().await,
        Err(crate::ReceiveError::ResyncRequired(
            crate::ResyncReason::Invalidated { .. }
        ))
    ));
}

#[tokio::test]
async fn cancelled_reconcile_caller_does_not_cancel_terminal_publication() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let mut events = harness.events().subscribe();
    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_outcome = HistoryOutcome::Failed;
        state.history_gate = Some(gate.clone());
    }
    let caller = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.reconcile(&id).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    gate.release.add_permits(1);
    assert!(
        matches!(timeout(Duration::from_secs(2), events.recv()).await.unwrap().unwrap().event, Event::RunFailed { run } if run.status == RunStatus::Failed)
    );
    assert_eq!(harness.get_run(&run.id).unwrap().status, RunStatus::Failed);
}

#[tokio::test]
async fn wait_reports_terminal_progress_for_externally_finalised_run() {
    let (_workspace, harness, _state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let mut events = harness.events().subscribe();
    // Simulate another process committing the terminal state on its own bus.
    harness
        .repository()
        .update_status(&run.id, RunStatus::Completed, None, None)
        .unwrap();
    let completed = harness.wait_for_run(&run.id).await.unwrap();
    assert_eq!(completed.status, RunStatus::Completed);
    loop {
        match timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap()
            .event
        {
            Event::RunProgress {
                run,
                backend_event: None,
            } => {
                assert_eq!(run.status, RunStatus::Completed);
                break;
            }
            Event::RunCompleted { .. } => panic!("the other process did not publish on this bus"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn cli_progress_subscription_keeps_every_update_even_when_broadcast_lags() {
    let (_workspace, harness, _state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let bus = EventBus::new(2);
    let mut progress = bus.subscribe_run_progress(&run.id);
    let mut live = bus.subscribe();
    let mut unrelated = run.clone();
    unrelated.id = "another-run".to_owned();
    bus.publish(Event::RunProgress {
        run: unrelated,
        backend_event: None,
    });
    for _ in 0..100 {
        bus.publish(Event::RunProgress {
            run: run.clone(),
            backend_event: None,
        });
    }
    assert!(matches!(
        live.recv().await,
        Err(crate::ReceiveError::ResyncRequired(
            crate::ResyncReason::Lagged { .. }
        ))
    ));
    for expected in 2..=101 {
        let event = progress.recv().await.unwrap();
        assert_eq!(event.id, expected);
        assert!(matches!(event.event, Event::RunProgress { run: seen, .. } if seen.id == run.id));
    }
    bus.invalidate();
    bus.publish(Event::RunProgress {
        run: run.clone(),
        backend_event: None,
    });
    assert!(matches!(
        progress.recv().await,
        Err(crate::ReceiveError::ResyncRequired(
            crate::ResyncReason::Invalidated { .. }
        ))
    ));
    assert!(
        matches!(progress.recv().await.unwrap().event, Event::RunProgress { run: seen, .. } if seen.id == run.id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_submission_response_cannot_undo_cancellation() {
    let (_workspace, harness, state, request) = setup(false);
    let mut events = harness.events().subscribe();
    let (gate, mut entered) = new_gate();
    state.lock().unwrap().submit_gate = Some(gate.clone());
    let submit = {
        let harness = harness.clone();
        tokio::spawn(async move { harness.submit(request).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let run = harness.list_runs(1).unwrap().remove(0);
    state.lock().unwrap().queue_position = Some(QueuePosition::Running);
    assert_eq!(
        timeout(Duration::from_secs(2), harness.cancel(&run.id))
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Cancelling
    );
    gate.release.add_permits(1);
    assert_eq!(submit.await.unwrap().unwrap().status, RunStatus::Cancelling);
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event.event, Event::RunProgress { run, .. } if run.status == RunStatus::Cancelling)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_while_reconcile_history_is_stalled() {
    for outcome in [HistoryOutcome::Unresolved, HistoryOutcome::Completed] {
        let (_workspace, harness, state, request) = setup(false);
        let run = harness.submit(request).await.unwrap();
        let (gate, mut entered) = new_gate();
        {
            let mut state = state.lock().unwrap();
            state.queue_position = Some(QueuePosition::Running);
            state.history_outcome = outcome;
            state.history_gate = Some(gate.clone());
        }
        let reconcile = {
            let harness = harness.clone();
            let id = run.id.clone();
            tokio::spawn(async move { harness.reconcile(&id).await })
        };
        timeout(Duration::from_secs(2), entered.recv())
            .await
            .unwrap()
            .unwrap();
        let cancelled = timeout(Duration::from_secs(2), harness.cancel(&run.id))
            .await
            .expect("cancel must not wait for history")
            .unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelling);
        assert!(state.lock().unwrap().interrupt_requested);
        gate.release.add_permits(1);
        let reconciled = timeout(Duration::from_secs(2), reconcile)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            reconciled.run.status,
            if outcome == HistoryOutcome::Completed {
                RunStatus::Completed
            } else {
                RunStatus::Cancelling
            }
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_while_output_download_is_stalled() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = Some(QueuePosition::Running);
        state.history_outcome = HistoryOutcome::Completed;
        state.download_gate = Some(gate.clone());
    }
    let reconcile = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.reconcile(&id).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let cancelled = timeout(Duration::from_secs(2), harness.cancel(&run.id))
        .await
        .expect("cancel must not wait for downloads")
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelling);
    assert!(state.lock().unwrap().interrupt_requested);
    gate.release.add_permits(1);
    assert_eq!(
        reconcile.await.unwrap().unwrap().run.status,
        RunStatus::Completed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_queue_observation_cannot_undo_cancellation_or_publish_started() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let mut events = harness.events().subscribe();
    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = Some(QueuePosition::Running);
        state.queue_once_gate = Some(gate.clone());
    }
    let reconcile = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.reconcile(&id).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), harness.cancel(&run.id))
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Cancelling
    );
    gate.release.add_permits(1);
    assert_eq!(
        reconcile.await.unwrap().unwrap().run.status,
        RunStatus::Cancelling
    );
    assert!(
        timeout(Duration::from_millis(100), events.recv())
            .await
            .is_err(),
        "stale start must not be published"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_interrupt_does_not_roll_back_a_concurrent_terminal_state() {
    let (_workspace, harness, state, request) = setup(false);
    let run = harness.submit(request).await.unwrap();
    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = Some(QueuePosition::Running);
        state.history_outcome = HistoryOutcome::Completed;
        state.interrupt_gate = Some(gate.clone());
        state.fail_interrupt = true;
    }
    let cancel = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.cancel(&id).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        harness.reconcile(&run.id).await.unwrap().run.status,
        RunStatus::Completed
    );
    gate.release.add_permits(1);
    assert!(cancel.await.unwrap().is_err());
    assert_eq!(
        harness.get_run(&run.id).unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_compare_and_set_deduplicates_across_separate_buses() {
    let (_workspace, harness, state, request) = setup(false);
    let other = Harness::with_event_bus(
        harness.config().clone(),
        harness.backend.clone(),
        EventBus::default(),
    )
    .unwrap();
    let run = harness.submit(request).await.unwrap();
    let mut first_events = harness.events().subscribe();
    let mut second_events = other.events().subscribe();
    let (gate, mut entered) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = Some(QueuePosition::Running);
        state.queue_gate = Some(gate.clone());
    }
    let first = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.reconcile(&id).await })
    };
    let second = {
        let other = other.clone();
        let id = run.id.clone();
        tokio::spawn(async move { other.reconcile(&id).await })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    gate.release.add_permits(2);
    assert_eq!(first.await.unwrap().unwrap().run.status, RunStatus::Running);
    assert_eq!(
        second.await.unwrap().unwrap().run.status,
        RunStatus::Running
    );
    let mut starts = 0;
    for receiver in [&mut first_events, &mut second_events] {
        if let Ok(result) = timeout(Duration::from_millis(100), receiver.recv()).await {
            assert!(matches!(result.unwrap().event, Event::RunStarted { .. }));
            starts += 1;
            assert!(timeout(Duration::from_millis(50), receiver.recv())
                .await
                .is_err());
        }
    }
    assert_eq!(starts, 1);
}

#[tokio::test]
async fn abandoned_reservation_is_recovered_without_resending() {
    for replay_first in [true, false] {
        let (_workspace, harness, state, request) = setup(false);
        let (gate, mut entered) = new_gate();
        state.lock().unwrap().submit_gate = Some(gate);
        let live = {
            let harness = harness.clone();
            let request = request.clone();
            tokio::spawn(async move {
                harness
                    .submit_keyed_until("durable-key".into(), request, &CancellationToken::new())
                    .await
            })
        };
        timeout(Duration::from_secs(2), entered.recv())
            .await
            .unwrap()
            .unwrap();
        let run = harness.list_runs(1).unwrap().remove(0);
        let config = harness.config().clone();
        let reopen = || {
            Harness::new(
                config.clone(),
                Arc::new(FakeImageBackend {
                    state: Arc::clone(&state),
                    ..Default::default()
                }),
            )
            .unwrap()
        };
        // Opening a second harness must not confuse a live owner with a crash.
        let observer = reopen();
        let replay = observer
            .submit_keyed("durable-key".into(), request.clone())
            .await
            .unwrap();
        assert_eq!(replay.run.status, RunStatus::Submitting);
        let result = observer.reconcile(&run.id).await.unwrap();
        assert_eq!(result.outcome, ReconcileOutcome::InProgressElsewhere);
        assert_eq!(result.run.status, RunStatus::Submitting);
        drop(observer);
        live.abort();
        assert!(matches!(live.await, Err(error) if error.is_cancelled()));
        drop(harness);
        state.lock().unwrap().queue_position = None;
        let reopened = reopen();
        if !replay_first {
            assert_eq!(
                reopened.reconcile(&run.id).await.unwrap().run.status,
                RunStatus::UnknownSubmission
            );
        }
        let replay = reopened
            .submit_keyed("durable-key".into(), request)
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.run.id, run.id);
        assert_eq!(replay.run.status, RunStatus::UnknownSubmission);
        assert_eq!(
            reopened.reconcile(&run.id).await.unwrap().run.status,
            RunStatus::UnknownSubmission
        );
        // The fake rejects any additional submission; the original is intact.
        assert_eq!(
            state.lock().unwrap().submission.as_ref().unwrap().2,
            run.prompt_id
        );
        assert_eq!(
            reopened
                .run_events(&run.id, 10)
                .unwrap()
                .iter()
                .filter(|event| event.event_type == "submission_unknown")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn failed_cancel_after_prompt_ack_restores_queued_and_preserves_warnings() {
    let (_workspace, harness, state, request) = setup(false);
    let (submit_gate, mut submitting) = new_gate();
    let (interrupt_gate, mut interrupting) = new_gate();
    {
        let mut state = state.lock().unwrap();
        state.submit_gate = Some(submit_gate.clone());
        state.interrupt_gate = Some(interrupt_gate.clone());
        state.submission_warnings = true;
        state.fail_interrupt = true;
    }
    let submit = {
        let harness = harness.clone();
        let request = request.clone();
        tokio::spawn(async move { harness.submit_keyed("ack-and-cancel".into(), request).await })
    };
    timeout(Duration::from_secs(2), submitting.recv())
        .await
        .unwrap()
        .unwrap();
    let run = harness.list_runs(1).unwrap().remove(0);
    state.lock().unwrap().queue_position = Some(QueuePosition::Running);
    let cancel = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.cancel(&id).await })
    };
    timeout(Duration::from_secs(2), interrupting.recv())
        .await
        .unwrap()
        .unwrap();
    submit_gate.release.add_permits(1);
    let accepted = submit.await.unwrap().unwrap().run;
    assert_eq!(accepted.status, RunStatus::Cancelling);
    assert_eq!(accepted.node_warnings, Some(json!({"node": "invalid"})));
    interrupt_gate.release.add_permits(1);
    assert!(cancel.await.unwrap().is_err());
    let replay = harness
        .submit_keyed("ack-and-cancel".into(), request)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.run.id, accepted.id);
    assert_eq!(replay.run.prompt_id, accepted.prompt_id);
    assert_eq!(replay.run.node_warnings, accepted.node_warnings);
    assert_eq!(replay.run.status, RunStatus::Queued);
    assert!(harness
        .run_events(&accepted.id, 10)
        .unwrap()
        .iter()
        .any(|event| event.event_type == "submission_acknowledged"
            && event.payload["node_errors"] == json!({"node": "invalid"})));
}

#[tokio::test]
async fn legacy_submitting_record_requires_backend_evidence() {
    let (_workspace, harness, state, request) = setup(false);
    let (gate, mut entered) = new_gate();
    state.lock().unwrap().submit_gate = Some(gate);
    let task = {
        let harness = harness.clone();
        let request = request.clone();
        tokio::spawn(async move {
            harness
                .submit_keyed_until("legacy".into(), request, &CancellationToken::new())
                .await
        })
    };
    timeout(Duration::from_secs(2), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let run = harness.list_runs(1).unwrap().remove(0);
    let config = harness.config().clone();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    drop(harness);
    let db = rusqlite::Connection::open(&config.state_database).unwrap();
    db.execute_batch("ALTER TABLE runs DROP COLUMN rerun_of; ALTER TABLE runs DROP COLUMN workflow_manifest_sha256; ALTER TABLE runs DROP COLUMN preflight_sha256; ALTER TABLE runs DROP COLUMN uses_submission_lock; ALTER TABLE runs DROP COLUMN submission_acknowledged; DROP TABLE run_media; DROP INDEX outputs_created_id_idx; PRAGMA user_version=5;").unwrap();
    drop(db);
    let lock_dir = config.state_database.with_extension("sqlite3.submissions");
    fs::remove_dir_all(&lock_dir).unwrap();
    let reopened = Harness::new(
        config,
        Arc::new(FakeImageBackend {
            state: state.clone(),
            ..Default::default()
        }),
    )
    .unwrap();
    state.lock().unwrap().queue_position = None;
    let replay = reopened
        .submit_keyed("legacy".into(), request)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.run.id, run.id);
    assert_eq!(replay.run.status, RunStatus::Submitting);
    assert_eq!(
        reopened.reconcile(&run.id).await.unwrap().run.status,
        RunStatus::Submitting
    );
    assert!(!lock_dir.exists());
    state.lock().unwrap().queue_position = Some(QueuePosition::Running);
    assert_eq!(
        reopened.reconcile(&run.id).await.unwrap().run.status,
        RunStatus::Running
    );
}
