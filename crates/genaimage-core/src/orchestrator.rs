use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    sync::mpsc,
    time::{interval, Duration},
};
use uuid::Uuid;

use crate::{
    artifacts::{self, InputFileRecord},
    comfyui::{ComfyUiClient, ComfyUiEvent, ComfyUiHealth},
    config::ResolvedConfig,
    persistence::{NewRun, OutputRecord, RunEvent, RunRecord, RunRepository, RunStatus},
    workflow::{LoadedWorkflow, WorkflowManifest},
    HarnessError, Result,
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

#[derive(Clone, Debug, Serialize)]
pub struct RunProgress {
    pub run: RunRecord,
    pub event: Option<ComfyUiEvent>,
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
    client: ComfyUiClient,
}

impl Harness {
    pub fn open(workspace: &Path) -> Result<Self> {
        let config = crate::HarnessConfig::load(workspace)?;
        let repository = RunRepository::open(&config.state_database)?;
        let client = ComfyUiClient::new(&config.comfyui)?;
        Ok(Self {
            config,
            repository,
            client,
        })
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub async fn health(&self) -> Result<ComfyUiHealth> {
        self.client.health().await
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

    pub async fn submit(&self, request: RunRequest) -> Result<RunRecord> {
        let loaded = self.load_workflow(&request.workflow)?;
        let mut bound = loaded.bind(request.preset.as_deref(), &request.parameters)?;
        self.client.validate_workflow_nodes(&bound.workflow).await?;
        let mut source_inputs = Vec::new();
        let mut uploaded_names = BTreeMap::new();
        for (parameter, relative_path) in &bound.image_uploads {
            let path = resolve_input_path(&self.config.workspace, relative_path);
            let upload = self.client.upload_image(&path).await?;
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

        let new_run = NewRun {
            id: run_id.clone(),
            prompt_id: prompt_id.clone(),
            client_id: client_id.clone(),
            workflow_id: loaded.manifest.id.clone(),
            workflow_version: loaded.manifest.version.clone(),
            workflow_sha256: loaded.workflow_sha256.clone(),
            generation_mode: loaded.manifest.generation_mode,
            status: RunStatus::Submitting,
            parameters: parameters.clone(),
            output_dir: output_dir.display().to_string(),
        };
        let mut run = self.repository.insert(&new_run)?;
        self.repository.record_event(
            &run.id,
            "run_created",
            Some("Run files and preflight validation completed"),
            &json!({
                "manifest_path": loaded.manifest_path,
                "preset": request.preset
            }),
        )?;

        match self
            .client
            .submit(&bound.workflow, &client_id, &prompt_id)
            .await
        {
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
                        "ComfyUI rejected one or more workflow nodes: {}",
                        response.node_errors
                    );
                    run = self.repository.update_status(
                        &run.id,
                        RunStatus::Failed,
                        Some(&error),
                        None,
                    )?;
                    self.repository.record_event(
                        &run.id,
                        "validation_failed",
                        Some(&error),
                        &response.node_errors,
                    )?;
                    return Ok(run);
                }
                if response.prompt_id != prompt_id {
                    run = self
                        .repository
                        .update_prompt_id(&run.id, &response.prompt_id)?;
                }
                run = self
                    .repository
                    .update_status(&run.id, RunStatus::Queued, None, None)?;
                self.repository.record_event(
                    &run.id,
                    "prompt_queued",
                    Some("ComfyUI accepted the prompt"),
                    &serde_json::to_value(response)?,
                )?;
            }
            Err(error) => {
                let message = error.to_string();
                run = self.repository.update_status(
                    &run.id,
                    RunStatus::UnknownSubmission,
                    Some(&message),
                    None,
                )?;
                self.repository.record_event(
                    &run.id,
                    "submission_unknown",
                    Some("Submission was not automatically retried"),
                    &json!({ "error": message }),
                )?;
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
        let mut run = self.repository.get(run_id)?;
        if run.status.is_terminal() {
            return Ok(ReconcileResult {
                outputs: self.repository.outputs(run_id)?,
                run,
            });
        }
        let queue = self.client.queue().await?;
        if queue.is_running(&run.prompt_id) {
            if run.status != RunStatus::Running {
                run = self
                    .repository
                    .update_status(run_id, RunStatus::Running, None, None)?;
                self.repository.record_event(
                    run_id,
                    "prompt_running",
                    Some("ComfyUI reports the prompt as executing"),
                    &serde_json::to_value(&queue)?,
                )?;
            }
        } else if queue.contains(&run.prompt_id) && run.status != RunStatus::Queued {
            run = self
                .repository
                .update_status(run_id, RunStatus::Queued, None, None)?;
            self.repository.record_event(
                run_id,
                "prompt_queued",
                Some("ComfyUI reports the prompt in the pending queue"),
                &json!({ "prompt_id": run.prompt_id }),
            )?;
        }

        let history = self.client.history(&run.prompt_id).await?;
        if history.complete {
            if run.status == RunStatus::Cancelling {
                let message =
                    "ComfyUI stopped the prompt after the cancellation request".to_owned();
                run = self.repository.update_status(
                    run_id,
                    RunStatus::Cancelled,
                    Some(&message),
                    None,
                )?;
                self.repository.record_event(
                    run_id,
                    "run_cancelled",
                    Some(&message),
                    &history.raw,
                )?;
            } else if history.success {
                let source_inputs = read_source_inputs(&PathBuf::from(&run.output_dir))?;
                let workflow_manifest_path = read_manifest_path(&PathBuf::from(&run.output_dir))?;
                let manifest_path = artifacts::materialise_history(
                    &self.client,
                    &self.repository,
                    &run,
                    &workflow_manifest_path,
                    source_inputs,
                    &history,
                )
                .await?;
                run = self.repository.update_status(
                    run_id,
                    RunStatus::Completed,
                    None,
                    Some(&manifest_path.display().to_string()),
                )?;
                self.repository.record_event(
                    run_id,
                    "run_completed",
                    Some("Outputs and provenance manifest were captured"),
                    &json!({
                        "output_count": history.outputs.len(),
                        "manifest_path": manifest_path
                    }),
                )?;
            } else {
                let error = format!(
                    "ComfyUI completed prompt '{}' without a successful result",
                    run.prompt_id
                );
                run =
                    self.repository
                        .update_status(run_id, RunStatus::Failed, Some(&error), None)?;
                self.repository
                    .record_event(run_id, "run_failed", Some(&error), &history.raw)?;
            }
        } else if run.status == RunStatus::Cancelling && !queue.contains(&run.prompt_id) {
            let message = "ComfyUI queue no longer contains the cancelled prompt";
            run =
                self.repository
                    .update_status(run_id, RunStatus::Cancelled, Some(message), None)?;
            self.repository.record_event(
                run_id,
                "run_cancelled",
                Some(message),
                &serde_json::to_value(&queue)?,
            )?;
        }
        Ok(ReconcileResult {
            outputs: self.repository.outputs(run_id)?,
            run,
        })
    }

    pub async fn reconcile_all(&self) -> Result<Vec<ReconcileResult>> {
        let mut results = Vec::new();
        for run in self.repository.active()? {
            results.push(self.reconcile(&run.id).await?);
        }
        Ok(results)
    }

    pub async fn wait_for_run(
        &self,
        run_id: &str,
        progress: Option<mpsc::UnboundedSender<RunProgress>>,
    ) -> Result<RunRecord> {
        let initial = self.repository.get(run_id)?;
        let mut monitor = self.client.monitor_events(&initial.client_id)?;
        let mut poll = interval(Duration::from_secs(1));
        loop {
            poll.tick().await;
            tokio::select! {
                event = monitor.receiver.recv() => {
                    if let Some(event) = event {
                        let relevant = event.prompt_id.as_deref().is_none()
                            || event.prompt_id.as_deref() == Some(initial.prompt_id.as_str());
                        if relevant {
                            self.repository.record_event(
                                run_id,
                                &event.event_type,
                                event.message.as_deref(),
                                &serde_json::to_value(&event)?,
                            )?;
                            let run = self.repository.get(run_id)?;
                            if let Some(sender) = &progress {
                                let _ = sender.send(RunProgress { run, event: Some(event) });
                            }
                        }
                    }
                }
                _ = poll.tick() => {
                    let result = self.reconcile(run_id).await?;
                    if let Some(sender) = &progress {
                        let _ = sender.send(RunProgress {
                            run: result.run.clone(),
                            event: None
                        });
                    }
                    if result.run.status.is_terminal() {
                        monitor.abort();
                        return Ok(result.run);
                    }
                }
            }
        }
    }

    pub async fn cancel(&self, run_id: &str) -> Result<RunRecord> {
        let run = self.repository.get(run_id)?;
        if run.status.is_terminal() {
            return Err(HarnessError::ComfyUi(format!(
                "run '{run_id}' is already {} and cannot be cancelled",
                run.status.as_str()
            )));
        }
        let queue = self.client.queue().await?;
        if queue.is_running(&run.prompt_id) {
            self.repository
                .update_status(run_id, RunStatus::Cancelling, None, None)?;
            let response = match self.client.interrupt().await {
                Ok(response) => response,
                Err(error) => {
                    self.repository.update_status(
                        run_id,
                        run.status,
                        Some(&error.to_string()),
                        None,
                    )?;
                    return Err(error);
                }
            };
            self.repository.record_event(
                run_id,
                "interrupt_requested",
                Some("ComfyUI was asked to interrupt the executing prompt"),
                &response,
            )?;
        } else if queue.contains(&run.prompt_id) {
            self.repository
                .update_status(run_id, RunStatus::Cancelling, None, None)?;
            let response = match self.client.delete_queued(&run.prompt_id).await {
                Ok(response) => response,
                Err(error) => {
                    self.repository.update_status(
                        run_id,
                        run.status,
                        Some(&error.to_string()),
                        None,
                    )?;
                    return Err(error);
                }
            };
            self.repository.record_event(
                run_id,
                "delete_requested",
                Some("ComfyUI was asked to remove the queued prompt"),
                &response,
            )?;
            return self.repository.get(run_id);
        } else {
            return Err(HarnessError::ComfyUi(format!(
                "prompt '{}' is not in the local ComfyUI queue; reconcile the run first",
                run.prompt_id
            )));
        }
        self.repository.get(run_id)
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
