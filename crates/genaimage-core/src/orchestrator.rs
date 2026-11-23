use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    artifacts::{self, InputFileRecord},
    config::{ResolvedConfig, SettingsUpdate},
    event::RunLockKind,
    image_backend::{BackendHealth, HistoryOutcome, ImageBackend, PromptResponse},
    persistence::{
        database_matches_settings, NewRun, OutputRecord, RunEvent, RunRecord, RunRepository,
        RunStatus, SubmissionReservation,
    },
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

/// Result of a durable submission and whether its key was already reserved.
pub struct KeyedSubmission {
    pub run: RunRecord,
    pub replayed: bool,
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

pub(crate) async fn before_deadline<T>(
    deadline: &CancellationToken,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = deadline.cancelled() => Err(HarnessError::ShuttingDown),
        result = operation => result,
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

    pub fn with_event_bus(
        config: ResolvedConfig,
        backend: Arc<dyn ImageBackend>,
        events: EventBus,
    ) -> Result<Self> {
        let repository = RunRepository::open(&config.state_database)?.with_identity(
            &config.workspace,
            backend.name(),
            backend.base_url(),
            config.claim_legacy_state,
        )?;
        let database_identity = fs::canonicalize(&config.state_database)
            .unwrap_or_else(|_| config.state_database.clone());
        Ok(Self {
            config,
            repository,
            backend,
            events,
            last_health: Arc::new(Mutex::new(None)),
            database_identity,
        })
    }

    /// Opens the state database on the blocking pool, using the supplied backend.
    pub async fn new_async(config: ResolvedConfig, backend: Arc<dyn ImageBackend>) -> Result<Self> {
        Self::with_event_bus_async(config, backend, EventBus::default()).await
    }

    pub async fn with_event_bus_async(
        config: ResolvedConfig,
        backend: Arc<dyn ImageBackend>,
        events: EventBus,
    ) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::with_event_bus(config, backend, events))
            .await
            .map_err(|error| {
                HarnessError::Database(format!("state database task failed: {error}"))
            })?
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

    /// Persists an update and prepares the live snapshot. Call on the blocking
    /// pool and serialise publishers. Listener, storage and endpoint identity
    /// are fixed until restart, when database ownership is checked again.
    pub fn apply_settings(&self, update: SettingsUpdate) -> Result<(ResolvedConfig, Self)> {
        if update.validate_database_path()? {
            let proposed = update.config();
            let endpoint = proposed.comfyui.endpoint_url()?;
            let path = &proposed.state_database;
            let compatible = database_matches_settings(
                path,
                proposed.workspace.to_str().expect("validated workspace"),
                self.backend.name(),
                endpoint.as_str(),
            )
            .map_err(|_| HarnessError::InvalidSetting {
                key: "paths.state_database".into(),
                value: path.display().to_string(),
                reason:
                    "use an empty SQLite database, a supported GenAImage database or a new file"
                        .into(),
            })?;
            if !compatible {
                return Err(HarnessError::SettingsDatabaseConflict {
                    key: if endpoint.as_str() != self.backend.base_url() { "comfyui.base_url" } else { "paths.state_database" },
                    reason: "The selected state database is owned by a different workspace or endpoint, or contains unclaimed legacy runs.",
                });
            }
        }
        let saved = update.persist()?;
        let mut live = self.clone();
        live.config.llm = saved.llm.clone();
        let timing = saved.comfyui.request_timeout_seconds;
        let reconnect = saved.comfyui.websocket_reconnect_seconds;
        if timing != self.config.comfyui.request_timeout_seconds
            || reconnect != self.config.comfyui.websocket_reconnect_seconds
        {
            if let Some(backend) = self.backend.with_timing(timing, reconnect) {
                live.backend = backend;
                live.config.comfyui.request_timeout_seconds = timing;
                live.config.comfyui.websocket_reconnect_seconds = reconnect;
            }
        }
        Ok((saved, live))
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

    /// Resolves an identifier only, without interpreting it as a filesystem path.
    pub fn load_workflow_by_id(&self, id: &str) -> Result<Option<LoadedWorkflow>> {
        let mut matches = Vec::new();
        let mut invalid_matches = Vec::new();
        visit_workflows(&self.config.workflows, |path, result| match result {
            Ok(loaded) if loaded.manifest.id == id => matches.push(loaded),
            Err(error) if sidecar_declares_id(path, id) => {
                invalid_matches.push((path.to_path_buf(), error));
            }
            _ => {}
        })?;
        if let Some((path, error)) = invalid_matches.iter().min_by_key(|(path, _)| path) {
            return Err(HarnessError::Workflow(format!(
                "cannot resolve workflow id '{id}': invalid manifest '{}' declares it ({error})",
                path.display()
            )));
        }
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.pop()),
            _ => Err(HarnessError::Workflow(format!(
                "workflow id '{id}' is ambiguous; resolve duplicate manifests"
            ))),
        }
    }

    /// Returns required node classes absent from the backend, or an error if
    /// the check could not be completed.
    pub async fn missing_workflow_nodes(&self, workflow: &Value) -> Result<Vec<String>> {
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
        Ok(class_types
            .into_iter()
            .filter(|class_type| !available.contains_key(class_type))
            .collect())
    }

    async fn validate_workflow_nodes(&self, workflow: &Value) -> Result<()> {
        let unavailable = self.missing_workflow_nodes(workflow).await?;
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
        Ok(self
            .submit_inner(request, None, &CancellationToken::new())
            .await?
            .run)
    }

    /// Submits a configured workflow by ID with a key scoped to the state database.
    /// Dropping the caller does not cancel the attempt.
    pub async fn submit_keyed(&self, key: String, request: RunRequest) -> Result<KeyedSubmission> {
        let harness = self.clone();
        tokio::spawn(async move {
            harness
                .submit_keyed_until(key, request, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Like `submit_keyed`, but the caller owns the task and can end network
    /// acknowledgement waits via `deadline`. Await it through durable finalisation.
    pub async fn submit_keyed_until(
        &self,
        key: String,
        request: RunRequest,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&request)?));
        let lock = self
            .events
            .run_lock(&self.database_identity, &key, RunLockKind::SubmitKey);
        let _key_guard = tokio::select! {
            biased;
            _ = deadline.cancelled() => return Err(HarnessError::ShuttingDown),
            guard = lock.lock() => guard,
        };
        let repository = self.repository.as_async();
        if let Some(run) = repository
            .find_submission(key.clone(), fingerprint.clone())
            .await?
        {
            return Ok(KeyedSubmission {
                run: self.recover_submission(run).await?.0,
                replayed: true,
            });
        }
        self.submit_inner(request, Some((key, fingerprint)), deadline)
            .await
    }

    // Acquire before publishing the run in SQLite. The OS releases ownership
    // on process death, even if a different Harness is serving this database.
    fn try_claim_submission(&self, run_id: &str) -> Result<Option<File>> {
        let mut directory = self.database_identity.as_os_str().to_os_string();
        directory.push(".submissions");
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        let name = hex::encode(Sha256::digest(run_id.as_bytes()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(name))?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(file)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Return the refreshed run and whether it is still owned by a live task.
    async fn recover_submission(&self, run: RunRecord) -> Result<(RunRecord, bool)> {
        if run.status != RunStatus::Submitting
            || !self
                .repository
                .as_async()
                .uses_submission_lock(&run.id)
                .await?
        {
            return Ok((run, false));
        }
        let Some(_owner) = self.try_claim_submission(&run.id)? else {
            return Ok((run, true));
        };
        let run = self
            .mark_submission_unknown(
                &run.id,
                "The submission owner exited before the prompt acknowledgement was recorded.",
            )
            .await?;
        Ok((run, false))
    }

    async fn submit_inner(
        &self,
        request: RunRequest,
        keyed: Option<(String, String)>,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        let repository = self.repository.as_async();
        let loaded = if keyed.is_some() {
            if !single_path_component(&request.workflow) {
                return Err(HarnessError::InvalidRunRequest(
                    "Select a configured workflow by ID, not by path.".into(),
                ));
            }
            if request.version.as_deref().is_some_and(|version| {
                !single_path_component(version)
                    || !version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            }) {
                return Err(HarnessError::InvalidRunRequest("Use a version containing only letters, digits, hyphens, underscores or dots, other than '.' and '..'.".into()));
            }
            self.load_workflow_by_id(&request.workflow)?
                .ok_or_else(|| {
                    HarnessError::Workflow(format!("workflow '{}' was not found", request.workflow))
                })?
        } else {
            self.load_workflow(&request.workflow)?
        };
        let mut bound = loaded.bind(request.preset.as_deref(), &request.parameters)?;
        if keyed.is_some() && !bound.image_uploads.is_empty() {
            return Err(HarnessError::InvalidRunRequest(
                "Server-side image file paths are not supported by the runs API.".into(),
            ));
        }
        tokio::select! {
            biased;
            _ = deadline.cancelled() => return Err(HarnessError::ShuttingDown),
            result = self.validate_workflow_nodes(&bound.workflow) => result?,
        }
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
        let run_guard = self
            .events
            .run_lock(&self.database_identity, &run_id, RunLockKind::Reconcile)
            .lock_owned()
            .await;
        let owner = self.try_claim_submission(&run_id)?.ok_or_else(|| {
            HarnessError::Database("new run already has a submission owner".into())
        })?;
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
        let is_keyed = keyed.is_some();
        let prepare_files = async {
            if is_keyed {
                fs::create_dir_all(self.config.runs.join(&loaded.manifest.id))?;
                fs::create_dir(&output_dir)?;
            } else {
                fs::create_dir_all(&output_dir)?;
            }
            artifacts::initialise_run_files(
                &output_dir,
                &bound,
                &loaded.manifest_path,
                request.preset.as_deref(),
                &parameters,
                &source_inputs,
            )
            .await
        };
        let run = if let Some((key, fingerprint)) = keyed {
            let run = match repository
                .reserve_submission(key, fingerprint, new_run)
                .await?
            {
                SubmissionReservation::Existing(run) => {
                    return Ok(KeyedSubmission {
                        run: self.recover_submission(run).await?.0,
                        replayed: true,
                    })
                }
                SubmissionReservation::New(run) => run,
            };
            // The mapping is already durable: never release it, even after a local failure.
            if let Err(error) = prepare_files.await {
                let message = error.to_string();
                let Some((run, transition)) = self
                    .transition_to_terminal(&run.id, RunStatus::Failed, Some(&message), None)
                    .await?
                else {
                    return Ok(KeyedSubmission {
                        run: repository.get(&run.id).await?,
                        replayed: false,
                    });
                };
                repository
                    .record_event(&run.id, "preparation_failed", Some(&message), &Value::Null)
                    .await?;
                transition.publish(Event::RunFailed { run: run.clone() });
                return Ok(KeyedSubmission {
                    run,
                    replayed: false,
                });
            }
            run
        } else {
            prepare_files.await?;
            repository.insert_owned_submission(new_run).await?
        };
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

        let response = tokio::select! {
            biased;
            _ = deadline.cancelled() => Err(HarnessError::ShuttingDown),
            response = self.backend.submit(&bound.workflow, &client_id, &prompt_id) => response,
        };
        let harness = self.clone();
        let run = tokio::spawn(async move {
            let (_guard, _owner) = (run_guard, owner);
            harness.finish_submission(run, response).await
        })
        .await
        .map_err(transition_task_error)??;
        Ok(KeyedSubmission {
            run,
            replayed: false,
        })
    }

    async fn finish_submission(
        &self,
        run: RunRecord,
        response: Result<PromptResponse>,
    ) -> Result<RunRecord> {
        let response =
            match response {
                Ok(response) if !response.prompt_id.trim().is_empty() => response,
                Ok(_) => {
                    return self
                        .mark_submission_unknown(
                            &run.id,
                            "The backend response did not contain a prompt ID.",
                        )
                        .await
                }
                Err(HarnessError::ShuttingDown) => return self
                    .mark_submission_unknown(
                        &run.id,
                        "Shutdown deadline expired before the prompt acknowledgement was recorded.",
                    )
                    .await,
                Err(error) => {
                    return self
                        .mark_submission_unknown(&run.id, &error.to_string())
                        .await
                }
            };
        let warnings = response
            .node_errors
            .as_object()
            .is_some_and(|errors| !errors.is_empty())
            || response
                .node_errors
                .as_array()
                .is_some_and(|errors| !errors.is_empty());
        let repository = self.repository.as_async();
        let transition = TransitionGuard::new(&self.events);
        let Some(run) = repository
            .acknowledge_submission(
                &run.id,
                response.prompt_id.clone(),
                warnings.then(|| response.node_errors.clone()),
            )
            .await?
        else {
            transition.disarm();
            return repository.get(&run.id).await;
        };
        let message = if warnings {
            " accepted the prompt with node warnings"
        } else {
            " accepted the prompt"
        };
        let queued = run.status == RunStatus::Queued;
        repository
            .record_event(
                &run.id,
                if queued {
                    "prompt_queued"
                } else {
                    "submission_acknowledged"
                },
                Some(&format!("{}{message}", self.backend.name())),
                &serde_json::to_value(response)?,
            )
            .await?;
        if queued {
            transition.publish(Event::RunQueued { run: run.clone() });
        } else {
            transition.publish(Event::RunProgress {
                run: run.clone(),
                backend_event: None,
            });
        }
        Ok(run)
    }

    async fn mark_submission_unknown(&self, id: &str, message: &str) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let transition = TransitionGuard::new(&self.events);
        let Some(run) = repository
            .compare_and_set_status(
                id,
                RunStatus::Submitting,
                RunStatus::UnknownSubmission,
                Some(message),
                None,
            )
            .await?
        else {
            transition.disarm();
            return repository.get(id).await;
        };
        repository
            .record_event(
                id,
                "submission_unknown",
                Some("Submission was not automatically retried"),
                &json!({"error": message}),
            )
            .await?;
        transition.publish(Event::RunUnknownSubmission { run: run.clone() });
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
        tokio::spawn(async move {
            harness
                .reconcile_until(&run_id, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Reconcile within the caller's task, stopping backend I/O at the deadline.
    pub async fn reconcile_until(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
    ) -> Result<ReconcileResult> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Reconcile);
        let _guard = before_deadline(deadline, async { Ok(lock.lock().await) }).await?;
        let repository = self.repository.as_async();
        let (mut run, live_owner) = self
            .recover_submission(repository.get(run_id).await?)
            .await?;
        if run.status.is_terminal() || live_owner {
            return Ok(ReconcileResult {
                outputs: repository.outputs(run_id).await?,
                run,
            });
        }
        let queue = before_deadline(deadline, self.backend.queue()).await?;
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

        let history = before_deadline(deadline, self.backend.history(&run.prompt_id)).await?;
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
                    deadline,
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

    /// Reconcile all active runs in the caller's task, with one shared deadline.
    pub async fn reconcile_all_until(
        &self,
        deadline: &CancellationToken,
    ) -> Result<Vec<ReconcileResult>> {
        let mut results = Vec::new();
        for run in self.repository.as_async().active().await? {
            results.push(self.reconcile_until(&run.id, deadline).await?);
        }
        Ok(results)
    }

    pub async fn wait_for_run(&self, run_id: &str) -> Result<RunRecord> {
        self.wait_for_run_inner(run_id, None).await
    }

    /// Stop observing when draining starts, and stop an in-flight poll at the deadline.
    pub async fn monitor_run(
        &self,
        run_id: &str,
        stop: &CancellationToken,
        deadline: &CancellationToken,
    ) -> Result<()> {
        match self
            .wait_for_run_inner(run_id, Some((stop, deadline)))
            .await
        {
            Ok(_) | Err(HarnessError::ShuttingDown) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn wait_for_run_inner(
        &self,
        run_id: &str,
        stop: Option<(&CancellationToken, &CancellationToken)>,
    ) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let initial = repository.get(run_id).await?;
        let stopped = async {
            match stop {
                Some((stop, _)) => stop.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(stopped);
        let mut monitor = tokio::select! {
            biased;
            _ = &mut stopped => return Ok(initial),
            result = self.backend.monitor_events(&initial.client_id) => result?,
        };
        let mut poll = interval(Duration::from_secs(1));
        poll.tick().await;
        let mut events_open = true;
        loop {
            tokio::select! {
                _ = &mut stopped => return repository.get(run_id).await,
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
                    let result = if let Some((_, deadline)) = stop {
                        self.reconcile_until(run_id, deadline).await?
                    } else {
                        self.reconcile(run_id).await?
                    };
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
        tokio::spawn(async move {
            harness
                .cancel_until(&run_id, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Cancel within the caller's task; only backend I/O is cut off at the deadline.
    pub async fn cancel_until(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
    ) -> Result<RunRecord> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Cancel);
        let _guard = before_deadline(deadline, async { Ok(lock.lock().await) }).await?;
        let repository = self.repository.as_async();
        let run = repository.get(run_id).await?;
        if run.status.is_terminal() {
            return Err(HarnessError::RunConflict(format!(
                "run '{run_id}' is already {} and cannot be cancelled",
                run.status.as_str()
            )));
        }
        let queue = before_deadline(deadline, self.backend.queue()).await?;
        let running = queue.is_running(&run.prompt_id);
        if !running && !queue.contains(&run.prompt_id) {
            return Err(HarnessError::RunConflict(format!(
                "prompt '{}' is not in the local {} queue; reconcile the run first",
                run.prompt_id,
                self.backend.name()
            )));
        }
        let previous = loop {
            let current = repository.get(run_id).await?;
            if current.status.is_terminal() {
                return Err(HarnessError::RunConflict(format!(
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
        let preparation = if running {
            before_deadline(deadline, self.backend.prepare_interrupt(&run.prompt_id)).await
        } else {
            Ok(())
        };
        if let Err(error) = preparation {
            self.finish_unsent_cancellation(run_id, previous, &error)
                .await?;
            return Err(error);
        }
        let mut attempted = false;
        let response = before_deadline(deadline, async {
            attempted = true;
            if running {
                self.backend.interrupt(&run.prompt_id).await
            } else {
                self.backend.delete_queued(&run.prompt_id).await
            }
        })
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                match &error {
                    HarnessError::ShuttingDown if !attempted => {
                        self.finish_unsent_cancellation(run_id, previous, &error)
                            .await?;
                    }
                    HarnessError::CancellationNotSent(_) => {
                        self.finish_unsent_cancellation(run_id, previous, &error)
                            .await?;
                    }
                    HarnessError::CancellationRejected(_) => {
                        repository
                            .rollback_cancellation(run_id, previous, error.to_string())
                            .await?;
                    }
                    _ => {
                        let cause = if matches!(error, HarnessError::ShuttingDown) {
                            "The shutdown deadline expired while waiting for cancellation acknowledgement.".into()
                        } else {
                            error.to_string()
                        };
                        repository.record_event(run_id, "cancellation_unknown",
                            Some("Cancellation was not confirmed; reconcile the run before taking further action."),
                            &json!({"prompt_id": run.prompt_id, "error": cause})).await?;
                        if !matches!(error, HarnessError::ShuttingDown) {
                            return Err(HarnessError::CancellationUnknown(cause));
                        }
                    }
                }
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

    async fn finish_unsent_cancellation(
        &self,
        id: &str,
        previous: RunStatus,
        error: &HarnessError,
    ) -> Result<()> {
        let repository = self.repository.as_async();
        repository
            .rollback_cancellation(id, previous, error.to_string())
            .await?;
        repository
            .record_event(
                id,
                "cancellation_not_sent",
                Some("The cancellation request was not sent."),
                &json!({"error": error.to_string()}),
            )
            .await
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

fn single_path_component(value: &str) -> bool {
    !value.is_empty() && !matches!(value, "." | "..") && !value.contains(['/', '\\', '\0'])
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
