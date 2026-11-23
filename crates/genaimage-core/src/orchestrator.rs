use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{interval, Duration};
use uuid::Uuid;

use crate::{
    artifacts::{self, InputFileRecord},
    config::ResolvedConfig,
    event::RunLockKind,
    image_backend::{BackendHealth, HistoryOutcome, ImageBackend, PromptResponse},
    persistence::{NewRun, OutputRecord, RunEvent, RunRecord, RunRepository, RunStatus},
    workflow::{GenerationMode, LoadedWorkflow, WorkflowManifest},
    Event, EventBus, HarnessError, Result,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunRequest {
    pub workflow: String,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileResult {
    pub run: RunRecord,
    pub outputs: Vec<OutputRecord>,
}

struct TransitionGuard {
    bus: EventBus,
    published: bool,
}

impl TransitionGuard {
    fn new(bus: &EventBus) -> Self {
        Self {
            bus: bus.clone(),
            published: false,
        }
    }

    fn disarm(mut self) {
        self.published = true;
    }

    fn publish(mut self, event: Event) {
        self.bus.publish(event);
        self.published = true;
    }
}

impl Drop for TransitionGuard {
    fn drop(&mut self) {
        if !self.published {
            self.bus.invalidate();
        }
    }
}

fn transition_task_error(error: tokio::task::JoinError) -> HarnessError {
    HarnessError::Database(format!("run transition task failed: {error}"))
}

#[derive(Debug, Default)]
pub struct WorkflowDiscovery {
    pub manifests: Vec<WorkflowManifest>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

#[derive(Debug)]
pub struct WorkflowDiagnostic {
    pub path: PathBuf,
    pub error: HarnessError,
}

#[derive(Clone)]
pub struct Harness {
    config: ResolvedConfig,
    repository: RunRepository,
    backend: Arc<dyn ImageBackend>,
    events: EventBus,
    last_health: Arc<Mutex<Option<Value>>>,
    database_identity: PathBuf,
}

impl Harness {
    pub fn new(config: ResolvedConfig, backend: Arc<dyn ImageBackend>) -> Result<Self> {
        Self::with_event_bus(config, backend, EventBus::default())
    }

    pub fn with_event_bus(config: ResolvedConfig, backend: Arc<dyn ImageBackend>, events: EventBus) -> Result<Self> {
        let repository = RunRepository::open(&config.state_database)?.with_identity(
            &config.workspace, backend.name(), backend.base_url(), config.claim_legacy_state,
        )?;
        let database_identity = fs::canonicalize(&config.state_database).unwrap_or_else(|_| config.state_database.clone());
        Ok(Self { config, repository, backend, events, last_health: Arc::new(Mutex::new(None)), database_identity })
    }

    /// Opens the state database on the blocking pool, using the supplied backend.
    pub async fn new_async(config: ResolvedConfig, backend: Arc<dyn ImageBackend>) -> Result<Self> {
        Self::with_event_bus_async(config, backend, EventBus::default()).await
    }

    pub async fn with_event_bus_async(config: ResolvedConfig, backend: Arc<dyn ImageBackend>, events: EventBus) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::with_event_bus(config, backend, events)).await
            .map_err(|error| HarnessError::Database(format!("state database task failed: {error}")))?
    }

    fn backend_error(&self, message: String) -> HarnessError {
        HarnessError::Backend {
            backend: self.backend.name().to_owned(),
            message,
        }
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub fn events(&self) -> &EventBus {
        &self.events
    }

    pub async fn health(&self) -> Result<BackendHealth> {
        let result = self.backend.health().await;
        let (health, error) = match &result {
            Ok(health) => (Some(health.clone()), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let current = json!({"health": health, "error": error});
        let mut previous = self.last_health.lock().expect("health mutex poisoned");
        if previous.as_ref() != Some(&current) {
            *previous = Some(current);
            self.events.publish(Event::BackendHealthChanged {
                backend: self.backend.name().to_owned(),
                health,
                error,
            });
        }
        result
    }

    pub fn repository(&self) -> &RunRepository {
        &self.repository
    }

    pub fn discover_workflows(&self) -> Result<WorkflowDiscovery> {
        let mut discovery = WorkflowDiscovery::default();
        visit_workflows(&self.config.workflows, |path, workflow| match workflow {
            Ok(loaded) => discovery.manifests.push(loaded.manifest),
            Err(error) => discovery.diagnostics.push(WorkflowDiagnostic {
                path: path.to_path_buf(),
                error,
            }),
        })?;
        discovery
            .manifests
            .sort_by(|left, right| left.id.cmp(&right.id));
        discovery
            .diagnostics
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(discovery)
    }

    pub fn load_workflow(&self, workflow: &str) -> Result<LoadedWorkflow> {
        let direct = Path::new(workflow);
        if direct.is_absolute() {
            return LoadedWorkflow::load(direct);
        }
        for candidate in [
            self.config.workspace.join(direct),
            self.config.workflows.join(direct),
        ] {
            if candidate.is_file() {
                return LoadedWorkflow::load(&candidate);
            }
        }

        let mut matches = Vec::new();
        let mut invalid_matches = Vec::new();
        let mut skipped = false;
        visit_workflows(&self.config.workflows, |path, result| match result {
            Ok(loaded) if loaded.manifest.id == workflow => matches.push(loaded),
            Err(error) => {
                skipped = true;
                if sidecar_declares_id(path, workflow) {
                    invalid_matches.push((path.to_path_buf(), error));
                }
            }
            _ => {}
        })?;
        if let Some((path, error)) = invalid_matches.iter().min_by_key(|(path, _)| path) {
            return Err(HarnessError::Workflow(format!(
                "cannot resolve workflow id '{workflow}': invalid manifest '{}' declares it ({error}); select an explicit manifest path",
                path.display()
            )));
        }
        if matches.is_empty() {
            for candidate in [
                self.config
                    .workflows
                    .join(format!("{workflow}.genaimage.json")),
                self.config
                    .workflows
                    .join(workflow)
                    .join("workflow.genaimage.json"),
            ] {
                if candidate.is_file() {
                    return LoadedWorkflow::load(&candidate);
                }
            }
        }
        match matches.len() {
            0 => Err(HarnessError::Workflow(format!(
                "workflow '{workflow}' was not found below '{}'{}",
                self.config.workflows.display(),
                if skipped {
                    "; some manifests could not be loaded; use 'workflow list' for details"
                } else {
                    ""
                }
            ))),
            1 => Ok(matches.remove(0)),
            _ => Err(HarnessError::Workflow(format!(
                "workflow id '{workflow}' is ambiguous; select an explicit manifest path"
            ))),
        }
    }

    async fn validate_workflow_nodes(&self, workflow: &Value) -> Result<()> {
        let nodes = workflow.as_object().ok_or_else(|| {
            HarnessError::Workflow("API workflow root must be an object".to_owned())
        })?;
        let class_types: BTreeSet<String> = nodes
            .values()
            .filter_map(|node| node.get("class_type").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let missing_class_types: Vec<&str> = nodes
            .iter()
            .filter_map(|(node_id, node)| {
                node.get("class_type")
                    .and_then(Value::as_str)
                    .is_none()
                    .then_some(node_id.as_str())
            })
            .collect();
        if !missing_class_types.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "nodes {} are missing class_type",
                missing_class_types.join(", ")
            )));
        }
        let available = self.backend.object_info().await?;
        let available = available.as_object().ok_or_else(|| {
            self.backend_error(format!(
                "{} /object_info did not return an object; cannot validate nodes",
                self.backend.name()
            ))
        })?;
        let unavailable: Vec<&str> = class_types
            .iter()
            .filter(|class_type| !available.contains_key(class_type.as_str()))
            .map(String::as_str)
            .collect();
        if !unavailable.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "{} does not provide required node types: {}",
                self.backend.name(),
                unavailable.join(", ")
            )));
        }
        Ok(())
    }

    pub async fn submit(&self, request: RunRequest) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let loaded = self.load_workflow(&request.workflow)?;
        let mut bound = loaded.bind(request.preset.as_deref(), &request.parameters)?;
        self.validate_workflow_nodes(&bound.workflow).await?;
        let mut source_inputs = Vec::new();
        let mut uploaded_names = BTreeMap::new();
        for (parameter, relative_path) in &bound.image_uploads {
            let path = resolve_input_path(&self.config.workspace, relative_path);
            let upload = self.backend.upload_image(&path).await?;
            let mut source = artifacts::checksum_file(&path)?;
            source.parameter = parameter.clone();
            source.uploaded_name = upload.name.clone();
            source_inputs.push(source);
            uploaded_names.insert(parameter.clone(), upload.name);
        }
        loaded.apply_uploaded_images(&mut bound, &uploaded_names)?;
        let parameters = serde_json::to_value(&bound.parameters)?;

        let run_id = format!("run-{}", Uuid::new_v4());
        let prompt_id = Uuid::new_v4().to_string();
        let client_id = format!("genaimage-{}", Uuid::new_v4());
        let output_dir = self.output_directory(&loaded, request.version.as_deref(), &run_id)?;
        fs::create_dir_all(&output_dir)?;
        artifacts::initialise_run_files(
            &output_dir,
            &bound,
            &loaded.manifest_path,
            request.preset.as_deref(),
            &parameters,
            &source_inputs,
        )
        .await?;

        let run_guard = self
            .events
            .run_lock(&self.database_identity, &run_id, RunLockKind::Reconcile)
            .lock_owned()
            .await;
        let new_run = NewRun {
            id: run_id.clone(),
            prompt_id: prompt_id.clone(),
            client_id: client_id.clone(),
            workflow_id: loaded.manifest.id.clone(),
            workflow_version: loaded.manifest.version.clone(),
            workflow_sha256: loaded.workflow_sha256.clone(),
            generation_mode: GenerationMode::Generic,
            status: RunStatus::Submitting,
            parameters: parameters.clone(),
            output_dir: output_dir.display().to_string(),
        };
        let run = repository.insert(&new_run).await?;
        repository
            .record_event(
                &run.id,
                "run_created",
                Some("Run files and preflight validation completed"),
                &json!({
                    "manifest_path": loaded.manifest_path,
                    "preset": request.preset
                }),
            )
            .await?;

        let response = self
            .backend
            .submit(&bound.workflow, &client_id, &prompt_id)
            .await;
        let harness = self.clone();
        tokio::spawn(async move {
            let _guard = run_guard;
            harness.finish_submission(run, &prompt_id, response).await
        })
        .await
        .map_err(transition_task_error)?
    }

    async fn finish_submission(
        &self,
        mut run: RunRecord,
        prompt_id: &str,
        response: Result<PromptResponse>,
    ) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        match response {
            Ok(response) => {
                let validation_failed = response
                    .node_errors
                    .as_object()
                    .is_some_and(|errors| !errors.is_empty())
                    || response
                        .node_errors
                        .as_array()
                        .is_some_and(|errors| !errors.is_empty());
                if validation_failed {
                    let error = format!(
                        "{} rejected one or more workflow nodes: {}",
                        self.backend.name(),
                        response.node_errors
                    );
                    let Some((updated, transition)) = self
                        .transition_to_terminal(&run.id, RunStatus::Failed, Some(&error), None)
                        .await?
                    else {
                        return repository.get(&run.id).await;
                    };
                    run = updated;
                    repository
                        .record_event(
                            &run.id,
                            "validation_failed",
                            Some(&error),
                            &response.node_errors,
                        )
                        .await?;
                    transition.publish(Event::RunFailed { run: run.clone() });
                    return Ok(run);
                }
                let transition = TransitionGuard::new(&self.events);
                if response.prompt_id != prompt_id {
                    run = repository
                        .update_prompt_id(&run.id, &response.prompt_id)
                        .await?;
                }
                let Some(updated) = repository
                    .compare_and_set_status(
                        &run.id,
                        RunStatus::Submitting,
                        RunStatus::Queued,
                        None,
                        None,
                    )
                    .await?
                else {
                    transition.disarm();
                    return repository.get(&run.id).await;
                };
                run = updated;
                repository
                    .record_event(
                        &run.id,
                        "prompt_queued",
                        Some(&format!("{} accepted the prompt", self.backend.name())),
                        &serde_json::to_value(response)?,
                    )
                    .await?;
                transition.publish(Event::RunQueued { run: run.clone() });
            }
            Err(error) => {
                let message = error.to_string();
                let transition = TransitionGuard::new(&self.events);
                let Some(updated) = repository
                    .compare_and_set_status(
                        &run.id,
                        RunStatus::Submitting,
                        RunStatus::UnknownSubmission,
                        Some(&message),
                        None,
                    )
                    .await?
                else {
                    transition.disarm();
                    return repository.get(&run.id).await;
                };
                run = updated;
                repository
                    .record_event(
                        &run.id,
                        "submission_unknown",
                        Some("Submission was not automatically retried"),
                        &json!({ "error": message }),
                    )
                    .await?;
                transition.publish(Event::RunUnknownSubmission { run: run.clone() });
            }
        }
        Ok(run)
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunRecord> {
        self.repository.get(run_id)
    }

    pub fn list_runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        self.repository.list(limit)
    }

    pub fn run_events(&self, run_id: &str, limit: usize) -> Result<Vec<RunEvent>> {
        self.repository.events(run_id, limit)
    }

    pub async fn reconcile(&self, run_id: &str) -> Result<ReconcileResult> {
        let harness = self.clone();
        let run_id = run_id.to_owned();
        tokio::spawn(async move { harness.reconcile_inner(&run_id).await })
            .await
            .map_err(transition_task_error)?
    }

    async fn reconcile_inner(&self, run_id: &str) -> Result<ReconcileResult> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Reconcile);
        let _guard = lock.lock().await;
        let repository = self.repository.as_async();
        let mut run = repository.get(run_id).await?;
        if run.status.is_terminal() {
            return Ok(ReconcileResult {
                outputs: repository.outputs(run_id).await?,
                run,
            });
        }
        let queue = self.backend.queue().await?;
        let transition_status = if queue.is_running(&run.prompt_id) {
            (!matches!(run.status, RunStatus::Running | RunStatus::Cancelling))
                .then_some(RunStatus::Running)
        } else if queue.contains(&run.prompt_id)
            && !matches!(run.status, RunStatus::Queued | RunStatus::Cancelling)
        {
            Some(RunStatus::Queued)
        } else {
            None
        };
        if let Some(status) = transition_status {
            let transition = TransitionGuard::new(&self.events);
            if let Some(updated) = repository
                .compare_and_set_status(run_id, run.status, status, None, None)
                .await?
            {
                run = updated;
                if status == RunStatus::Running {
                    repository
                        .record_event(
                            run_id,
                            "prompt_running",
                            Some(&format!(
                                "{} reports the prompt as executing",
                                self.backend.name()
                            )),
                            &serde_json::to_value(&queue)?,
                        )
                        .await?;
                    transition.publish(Event::RunStarted { run: run.clone() });
                } else {
                    repository
                        .record_event(
                            run_id,
                            "prompt_queued",
                            Some(&format!(
                                "{} reports the prompt in the pending queue",
                                self.backend.name()
                            )),
                            &json!({"prompt_id": run.prompt_id}),
                        )
                        .await?;
                    transition.publish(Event::RunQueued { run: run.clone() });
                }
            } else {
                transition.disarm();
                run = repository.get(run_id).await?;
                if run.status.is_terminal() {
                    return Ok(ReconcileResult {
                        outputs: repository.outputs(run_id).await?,
                        run,
                    });
                }
            }
        }

        let history = self.backend.history(&run.prompt_id).await?;
        if history.prompt_id != run.prompt_id {
            return Err(self.backend_error(format!(
                "requested history for prompt '{}', but received history for '{}'",
                run.prompt_id, history.prompt_id
            )));
        }
        match history.outcome {
            HistoryOutcome::Unresolved => {}
            HistoryOutcome::Cancelled => {
                let message = format!(
                    "{} confirmed cancellation of the prompt",
                    self.backend.name()
                );
                if let Some((updated, transition)) = self
                    .transition_to_terminal(run_id, RunStatus::Cancelled, Some(&message), None)
                    .await?
                {
                    repository
                        .record_event(run_id, "run_cancelled", Some(&message), &history.raw)
                        .await?;
                    transition.publish(Event::RunCancelled { run: updated });
                }
            }
            HistoryOutcome::Completed => {
                let source_inputs = read_source_inputs(&PathBuf::from(&run.output_dir))?;
                let workflow_manifest_path = read_manifest_path(&PathBuf::from(&run.output_dir))?;
                let manifest_path = artifacts::materialise_history(
                    self.backend.as_ref(),
                    &repository,
                    &run,
                    &workflow_manifest_path,
                    source_inputs,
                    &history,
                )
                .await?;
                if let Some((updated, transition)) = self
                    .transition_to_terminal(
                        run_id,
                        RunStatus::Completed,
                        None,
                        Some(&manifest_path.display().to_string()),
                    )
                    .await?
                {
                    for output in repository.outputs(run_id).await? {
                        self.events.publish(Event::RunOutput {
                            run_id: run_id.to_owned(),
                            output,
                        });
                    }
                    repository.record_event(
                        run_id, "run_completed", Some("Outputs and provenance manifest were captured"),
                        &json!({"output_count": history.outputs.len(), "manifest_path": manifest_path}),
                    ).await?;
                    transition.publish(Event::RunCompleted { run: updated });
                }
            }
            HistoryOutcome::Failed => {
                let error = format!(
                    "{} completed prompt '{}' without a successful result",
                    self.backend.name(),
                    run.prompt_id
                );
                if let Some((updated, transition)) = self
                    .transition_to_terminal(run_id, RunStatus::Failed, Some(&error), None)
                    .await?
                {
                    repository
                        .record_event(run_id, "run_failed", Some(&error), &history.raw)
                        .await?;
                    transition.publish(Event::RunFailed { run: updated });
                }
            }
        }
        Ok(ReconcileResult {
            outputs: repository.outputs(run_id).await?,
            run: repository.get(run_id).await?,
        })
    }

    async fn transition_to_terminal(
        &self,
        run_id: &str,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<Option<(RunRecord, TransitionGuard)>> {
        let repository = self.repository.as_async();
        loop {
            let current = repository.get(run_id).await?;
            if current.status.is_terminal() {
                return Ok(None);
            }
            let guard = TransitionGuard::new(&self.events);
            match repository
                .compare_and_set_status(run_id, current.status, status, error, manifest_path)
                .await?
            {
                Some(updated) => return Ok(Some((updated, guard))),
                None => guard.disarm(),
            }
        }
    }

    pub async fn reconcile_all(&self) -> Result<Vec<ReconcileResult>> {
        let repository = self.repository.as_async();
        let mut results = Vec::new();
        for run in repository.active().await? {
            results.push(self.reconcile(&run.id).await?);
        }
        Ok(results)
    }

    pub async fn wait_for_run(&self, run_id: &str) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let initial = repository.get(run_id).await?;
        let mut monitor = self.backend.monitor_events(&initial.client_id).await?;
        let mut poll = interval(Duration::from_secs(1));
        poll.tick().await;
        let mut events_open = true;
        loop {
            tokio::select! {
                event = monitor.recv(), if events_open => {
                    if let Some(event) = event {
                        let relevant = event.prompt_id.as_deref().is_none()
                            || event.prompt_id.as_deref() == Some(initial.prompt_id.as_str());
                        if relevant {
                            repository.record_event(
                                run_id,
                                &event.event_type,
                                event.message.as_deref(),
                                &serde_json::to_value(&event)?,
                            ).await?;
                            let run = repository.get(run_id).await?;
                            self.events.publish(Event::RunProgress { run, backend_event: Some(event) });
                        }
                    } else {
                        events_open = false;
                    }
                }
                _ = poll.tick() => {
                    let result = self.reconcile(run_id).await?;
                    self.events.publish(Event::RunProgress { run: result.run.clone(), backend_event: None });
                    if result.run.status.is_terminal() {
                        return Ok(result.run);
                    }
                }
            }
        }
    }

    pub async fn cancel(&self, run_id: &str) -> Result<RunRecord> {
        let harness = self.clone();
        let run_id = run_id.to_owned();
        tokio::spawn(async move { harness.cancel_inner(&run_id).await })
            .await
            .map_err(transition_task_error)?
    }

    async fn cancel_inner(&self, run_id: &str) -> Result<RunRecord> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Cancel);
        let _guard = lock.lock().await;
        let repository = self.repository.as_async();
        let run = repository.get(run_id).await?;
        if run.status.is_terminal() {
            return Err(self.backend_error(format!(
                "run '{run_id}' is already {} and cannot be cancelled",
                run.status.as_str()
            )));
        }
        let queue = self.backend.queue().await?;
        let running = queue.is_running(&run.prompt_id);
        if !running && !queue.contains(&run.prompt_id) {
            return Err(self.backend_error(format!(
                "prompt '{}' is not in the local {} queue; reconcile the run first",
                run.prompt_id,
                self.backend.name()
            )));
        }
        let previous = loop {
            let current = repository.get(run_id).await?;
            if current.status.is_terminal() {
                return Err(self.backend_error(format!(
                    "run '{run_id}' is already {} and cannot be cancelled",
                    current.status.as_str()
                )));
            }
            if repository
                .compare_and_set_status(run_id, current.status, RunStatus::Cancelling, None, None)
                .await?
                .is_some()
            {
                break current.status;
            }
        };
        let response = if running {
            self.backend.interrupt().await
        } else {
            self.backend.delete_queued(&run.prompt_id).await
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                repository
                    .compare_and_set_status(
                        run_id,
                        RunStatus::Cancelling,
                        previous,
                        Some(&error.to_string()),
                        None,
                    )
                    .await?;
                return Err(error);
            }
        };
        let (event_type, message) = if running {
            (
                "interrupt_requested",
                format!(
                    "{} was asked to interrupt the executing prompt",
                    self.backend.name()
                ),
            )
        } else {
            (
                "delete_requested",
                format!(
                    "{} was asked to remove the queued prompt",
                    self.backend.name()
                ),
            )
        };
        repository
            .record_event(run_id, event_type, Some(&message), &response)
            .await?;
        repository.get(run_id).await
    }

    fn output_directory(
        &self,
        workflow: &LoadedWorkflow,
        version: Option<&str>,
        run_id: &str,
    ) -> Result<PathBuf> {
        let version = version.unwrap_or(run_id);
        let safe_version: String = version
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        if safe_version.is_empty() {
            return Err(HarnessError::Configuration(
                "output version cannot be empty".to_owned(),
            ));
        }
        Ok(self
            .config
            .runs
            .join(&workflow.manifest.id)
            .join(safe_version))
    }
}

fn resolve_input_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn read_source_inputs(output_dir: &Path) -> Result<Vec<InputFileRecord>> {
    let value: Value = serde_json::from_slice(&fs::read(output_dir.join("preflight.json"))?)?;
    serde_json::from_value(
        value
            .get("source_inputs")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(HarnessError::Json)
}

fn read_manifest_path(output_dir: &Path) -> Result<PathBuf> {
    let value: Value = serde_json::from_slice(&fs::read(output_dir.join("preflight.json"))?)?;
    value
        .get("workflow_manifest_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            HarnessError::Workflow("preflight.json is missing workflow_manifest_path".to_owned())
        })
}

fn sidecar_declares_id(path: &Path, id: &str) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|manifest| manifest.get("id").and_then(Value::as_str) == Some(id))
}

fn visit_workflows(
    directory: &Path,
    mut visitor: impl FnMut(&Path, Result<LoadedWorkflow>),
) -> Result<()> {
    visit_files(directory, &mut |path| {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".genaimage.json"))
        {
            visitor(path, LoadedWorkflow::load(path));
        }
    })
}

fn visit_files(directory: &Path, visitor: &mut dyn FnMut(&Path)) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            visit_files(&path, visitor)?;
        } else {
            visitor(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
