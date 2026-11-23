use std::sync::Mutex;

use tempfile::TempDir;
use tokio::time::timeout;

use super::*;
use crate::{
    async_trait,
    image_backend::{
        EventMonitor, HistorySnapshot, OutputReference, PromptResponse, QueueSnapshot,
        UploadResponse,
    },
    ExtensionRegistry, HarnessConfig,
};

const IMAGE: &[u8] = b"fake image output";

#[derive(Default)]
struct FakeImageBackend {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    lose_submit_response: bool,
    submission: Option<(Value, String, String)>,
    queue_position: Option<QueuePosition>,
    history_success: Option<bool>,
    delete_requested: bool,
    interrupt_requested: bool,
    downloads: usize,
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
        "fake://local"
    }

    async fn health(&self) -> Result<BackendHealth> {
        Ok(BackendHealth {
            system: json!({"ready": true}),
        })
    }

    async fn object_info(&self) -> Result<Value> {
        Ok(json!({"KSampler": {}, "SaveImage": {}}))
    }

    async fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> Result<PromptResponse> {
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
        Ok(PromptResponse {
            prompt_id: prompt_id.to_owned(),
            number: None,
            node_errors: Value::Null,
        })
    }

    async fn queue(&self) -> Result<QueueSnapshot> {
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
        let state = self.state.lock().unwrap();
        assert_eq!(prompt_id, state.submission.as_ref().unwrap().2);
        let success = state.history_success == Some(true);
        Ok(HistorySnapshot {
            prompt_id: prompt_id.to_owned(),
            complete: state.history_success.is_some(),
            success,
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
            raw: json!({"id": prompt_id, "success": state.history_success}),
        })
    }

    async fn delete_queued(&self, prompt_id: &str) -> Result<Value> {
        let mut state = self.state.lock().unwrap();
        assert_eq!(prompt_id, state.submission.as_ref().unwrap().2);
        assert!(matches!(state.queue_position, Some(QueuePosition::Pending)));
        state.delete_requested = true;
        Ok(Value::Null)
    }

    async fn interrupt(&self) -> Result<Value> {
        let mut state = self.state.lock().unwrap();
        assert!(matches!(state.queue_position, Some(QueuePosition::Running)));
        state.interrupt_requested = true;
        Ok(Value::Null)
    }

    async fn upload_image(&self, _path: &Path) -> Result<UploadResponse> {
        panic!("the fixture has no image inputs to upload")
    }

    async fn download_output(&self, output: &OutputReference) -> Result<Vec<u8>> {
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

fn setup(lose_submit_response: bool) -> (TempDir, Harness, Arc<Mutex<FakeState>>, RunRequest) {
    let workspace = tempfile::tempdir().unwrap();
    let mut config = HarnessConfig::load(workspace.path()).unwrap();
    config.active_profile = "test-backend".to_owned();
    config.state_database = workspace.path().join("state.sqlite3");
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
        })
    })
    .unwrap();
    assert!(registry.image_backend("local").is_none());
    let backend = registry.image_backend(&config.active_profile).unwrap();
    let harness = Harness::new(config, backend).unwrap();
    let request = RunRequest {
        workflow: "contract-workflow".to_owned(),
        preset: Some("fast".to_owned()),
        parameters: BTreeMap::from([("seed".to_owned(), json!(123))]),
        version: None,
    };
    (workspace, harness, state, request)
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
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let waiter = {
        let harness = harness.clone();
        let id = run.id.clone();
        tokio::spawn(async move { harness.wait_for_run(&id, Some(sender)).await.unwrap() })
    };
    timeout(Duration::from_secs(3), async {
        let mut progress_seen = false;
        let mut running_seen = false;
        while !progress_seen || !running_seen {
            let update = receiver.recv().await.unwrap();
            if let Some(event) = update.event {
                assert_eq!(event.prompt_id.as_deref(), Some(run.prompt_id.as_str()));
                assert_eq!(event.value, Some(4));
                assert!(!progress_seen, "only one event belongs to this run");
                progress_seen = true;
            }
            running_seen |= update.run.status == RunStatus::Running;
        }
    })
    .await
    .unwrap();
    {
        let mut state = state.lock().unwrap();
        state.queue_position = None;
        state.history_success = Some(true);
    }
    let completed = timeout(Duration::from_secs(3), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, RunStatus::Completed);
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
        state.history_success = Some(false);
    }
    let result = harness.reconcile(&run.id).await.unwrap();
    assert_eq!(result.run.status, RunStatus::Failed);
    assert!(result.run.manifest_path.is_none());
    assert!(result.outputs.is_empty());
    assert_eq!(state.lock().unwrap().downloads, 0);
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
            if matches!(position, QueuePosition::Running) {
                state.history_success = Some(false);
            }
        }
        let result = harness.reconcile(&run.id).await.unwrap();
        assert_eq!(result.run.status, RunStatus::Cancelled);
        assert!(result.outputs.is_empty());
    }
}

#[tokio::test]
async fn lost_submit_response_stays_unknown_until_queue_or_history_matches() {
    for seen_in_queue in [false, true] {
        let (_workspace, harness, state, request) = setup(true);
        let run = harness.submit(request).await.unwrap();
        assert_eq!(run.status, RunStatus::UnknownSubmission);
        for _ in 0..2 {
            assert_eq!(
                harness.reconcile(&run.id).await.unwrap().run.status,
                RunStatus::UnknownSubmission
            );
        }
        if seen_in_queue {
            state.lock().unwrap().queue_position = Some(QueuePosition::Pending);
            assert_eq!(
                harness.reconcile(&run.id).await.unwrap().run.status,
                RunStatus::Queued
            );
        }
        {
            let mut state = state.lock().unwrap();
            state.queue_position = None;
            state.history_success = Some(true);
        }
        let result = harness.reconcile(&run.id).await.unwrap();
        assert_eq!(result.run.status, RunStatus::Completed);
        assert_eq!(result.run.prompt_id, run.prompt_id);
        assert_eq!(result.outputs.len(), 1);
    }
}

#[test]
fn registry_does_not_replace_an_extension_or_fall_back_on_factory_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let config = HarnessConfig::load(workspace.path()).unwrap();
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
    let original = registry.image_backend(&config.active_profile).unwrap();
    assert!(registry
        .register_image_backend(
            &config.active_profile,
            Arc::new(FakeImageBackend::default())
        )
        .is_err());
    assert!(Arc::ptr_eq(
        &original,
        &registry.image_backend(&config.active_profile).unwrap()
    ));
}
