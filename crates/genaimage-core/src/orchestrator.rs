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
    gates::CharacterState,
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
    pub character_id: Option<String>,
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

    pub fn discover_workflows(&self) -> Result<Vec<WorkflowManifest>> {
        if !self.config.workflows.exists() {
            return Ok(Vec::new());
        }
        let mut manifests = Vec::new();
        visit_files(&self.config.workflows, &mut |path| {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".genaimage.json"))
            {
                let loaded = LoadedWorkflow::load(path)?;
                manifests.push(loaded.manifest);
            }
            Ok(())
        })?;
        manifests.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(manifests)
    }

    pub fn load_workflow(&self, workflow: &str) -> Result<LoadedWorkflow> {
        let direct = PathBuf::from(workflow);
        let candidates = if direct.is_absolute() {
            vec![direct]
        } else {
            vec![
                self.config.workspace.join(&direct),
                self.config.workflows.join(&direct),
                self.config
                    .workflows
                    .join(format!("{workflow}.genaimage.json")),
                self.config
                    .workflows
                    .join(workflow)
                    .join("workflow.genaimage.json"),
            ]
        };
        for candidate in candidates {
            if candidate.is_file() {
                return LoadedWorkflow::load(&candidate);
            }
        }
        let mut matches = Vec::new();
        visit_files(&self.config.workflows, &mut |path| {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".genaimage.json"))
            {
                let loaded = LoadedWorkflow::load(path)?;
                if loaded.manifest.id == workflow {
                    matches.push(loaded);
                }
            }
            Ok(())
        })?;
        match matches.len() {
            0 => Err(HarnessError::Workflow(format!(
                "workflow '{workflow}' was not found below '{}'",
                self.config.workflows.display()
            ))),
            1 => Ok(matches.remove(0)),
            _ => Err(HarnessError::Workflow(format!(
                "workflow id '{workflow}' is ambiguous; select an explicit manifest path"
            ))),
        }
    }

    pub async fn submit(&self, request: RunRequest) -> Result<RunRecord> {
        let loaded = self.load_workflow(&request.workflow)?;
        let required_gates = loaded.required_gates();
        let (character_state, gate_snapshot) = if required_gates.is_empty() {
            (None, None)
        } else {
            let character_id = request.character_id.as_deref().ok_or_else(|| {
                HarnessError::Gate(format!(
                    "workflow '{}' requires a character context for {}",
                    loaded.manifest.id,
                    required_gates
                        .iter()
                        .map(|gate| gate.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            let character_dir = self.character_dir(character_id)?;
            let state = CharacterState::load(&character_dir)?;
            state.assert_approved(&required_gates)?;
            let snapshot = serde_json::to_value(&state)?;
            (
                Some((character_id.to_owned(), character_dir, state)),
                Some(snapshot),
            )
        };

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
        let output_dir = self.output_directory(
            &loaded,
            character_state
                .as_ref()
                .map(|(_, directory, _)| directory.as_path()),
            request.version.as_deref(),
            &run_id,
        )?;
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
            character_id: character_state.map(|(id, _, _)| id),
            status: RunStatus::Submitting,
            parameters: parameters.clone(),
            gate_snapshot,
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

    pub fn initialise_character(
        &self,
        character_id: &str,
        name: Option<String>,
    ) -> Result<CharacterState> {
        let directory = self.character_dir(character_id)?;
        if CharacterState::state_path(&directory).exists() {
            return Err(HarnessError::Configuration(format!(
                "character '{character_id}' is already initialised at '{}'",
                directory.display()
            )));
        }
        fs::create_dir_all(directory.join("specifications"))?;
        fs::create_dir_all(directory.join("references"))?;
        fs::create_dir_all(directory.join("briefs"))?;
        fs::create_dir_all(directory.join("generations"))?;
        fs::create_dir_all(directory.join("reviews"))?;
        fs::create_dir_all(directory.join("delivery"))?;
        self.copy_character_template(
            "production-record.md",
            &directory.join("production-record.md"),
        )?;
        self.copy_character_template("input-package.md", &directory.join("input-package.md"))?;
        self.copy_character_template(
            "design-specification.md",
            &directory.join("specifications/design-specification.md"),
        )?;
        self.copy_character_template("visual-brief.md", &directory.join("briefs/visual-brief.md"))?;
        self.copy_character_template(
            "revision-review.md",
            &directory.join("reviews/revision-review.md"),
        )?;
        let state = CharacterState::new(character_id, name);
        state.save(&directory)?;
        Ok(state)
    }

    pub fn character_status(&self, character_id: &str) -> Result<CharacterState> {
        CharacterState::load(&self.character_dir(character_id)?)
    }

    pub fn decide_character_gate(
        &self,
        character_id: &str,
        gate: crate::GateId,
        status: crate::ApprovalStatus,
        approver: String,
        decision: String,
    ) -> Result<CharacterState> {
        let directory = self.character_dir(character_id)?;
        let mut state = CharacterState::load(&directory)?;
        state.decide(gate, status, approver, decision)?;
        state.save(&directory)?;
        Ok(state)
    }

    fn character_dir(&self, character_id: &str) -> Result<PathBuf> {
        if character_id.is_empty()
            || character_id.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            })
        {
            return Err(HarnessError::InvalidPath {
                path: PathBuf::from(character_id),
                reason: "character ID must contain only ASCII letters, digits, '-' or '_'"
                    .to_owned(),
            });
        }
        Ok(self.config.characters.join(character_id))
    }

    fn copy_character_template(&self, template_name: &str, target: &Path) -> Result<()> {
        let source = self
            .config
            .workspace
            .join("templates/character-concept-art")
            .join(template_name);
        if target.exists() {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if source.exists() {
            fs::copy(source, target)?;
        } else {
            let title = match template_name {
                "production-record.md" => "Character Production Record",
                "input-package.md" => "Character Input Package",
                "design-specification.md" => "Character Design Specification",
                "visual-brief.md" => "Visual Generation Brief",
                "revision-review.md" => "Revision Review",
                _ => "Character Record",
            };
            fs::write(
                target,
                format!(
                    "# {title}\n\nCreated by GenAImage. Follow \
                     `workflows/character-concept-art/WORKFLOW.md` and preserve all prior versions.\n"
                ),
            )?;
        }
        Ok(())
    }

    fn output_directory(
        &self,
        workflow: &LoadedWorkflow,
        character_dir: Option<&Path>,
        version: Option<&str>,
        run_id: &str,
    ) -> Result<PathBuf> {
        let version = version.unwrap_or(run_id);
        let safe_version: String = version
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
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
        if let Some(character_dir) = character_dir {
            Ok(character_dir
                .join("generations")
                .join(safe_version)
                .join(run_id))
        } else {
            Ok(self
                .config
                .runs
                .join(&workflow.manifest.id)
                .join(safe_version))
        }
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

fn visit_files(directory: &Path, visitor: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            visit_files(&path, visitor)?;
        } else {
            visitor(&path)?;
        }
    }
    Ok(())
}
