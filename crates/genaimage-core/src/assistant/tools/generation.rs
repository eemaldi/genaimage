//! Tools that start or stop generation. They follow the portal's path: the
//! reviewed workflow revision, checkpoint profile revision and character spec
//! version are pinned, every effective value is shown before submission, and
//! the confirmation ID is the idempotency key.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{bounded_tool_result, load_workflow, proposal_error, run_id_schema, RunIdArguments};
use crate::{
    checkpoint_profiles::LayeredBindings, workflow::ValueSource, AssistantTool, CharacterLink,
    Harness, HarnessError, KeyedSubmission, LoadedWorkflow, Result, RunRecord, RunRequest,
    RunStatus, ToolContext,
};

pub(super) fn generation_tools(harness: &Arc<Harness>) -> Vec<Arc<dyn AssistantTool>> {
    vec![
        Arc::new(ProposeRunTool(Arc::clone(harness))),
        Arc::new(SubmitRunTool(Arc::clone(harness))),
        Arc::new(CancelRunTool(Arc::clone(harness))),
    ]
}

/// A reviewed single-run request. Profile and character are explicit and
/// optional; nothing is inferred when they are absent.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArguments {
    workflow_id: String,
    workflow_revision: String,
    #[serde(default)]
    preset: Option<String>,
    parameters: BTreeMap<String, Value>,
    #[serde(default)]
    checkpoint_profile: Option<String>,
    #[serde(default)]
    checkpoint_profile_revision: Option<String>,
    #[serde(default)]
    character: Option<CharacterLink>,
}

impl RunArguments {
    /// The request exactly as reviewed: user values only, so that the core
    /// layers the profile itself and records where each value came from.
    fn request(&self) -> RunRequest {
        RunRequest {
            workflow: self.workflow_id.clone(),
            workflow_revision: Some(self.workflow_revision.clone()),
            preset: self.preset.clone(),
            parameters: self.parameters.clone(),
            version: None,
            checkpoint_profile: self.checkpoint_profile.clone(),
            checkpoint_profile_revision: self.checkpoint_profile_revision.clone(),
            character: self.character.clone(),
        }
    }

    /// A chat always has a review step, so a profile is named together with
    /// the revision the user saw, even though the HTTP API allows omitting it.
    fn check_profile_pairing(&self) -> Result<()> {
        match (&self.checkpoint_profile, &self.checkpoint_profile_revision) {
            (Some(_), None) => Err(HarnessError::InvalidToolArguments(
                "checkpoint_profile must be named together with checkpoint_profile_revision, both taken from inspect_workflow or list_checkpoints; a profile is never applied without the revision that was reviewed".into(),
            )),
            (None, Some(_)) => Err(HarnessError::InvalidToolArguments(
                "checkpoint_profile_revision was given without checkpoint_profile; name the profile it belongs to or drop the revision".into(),
            )),
            _ => Ok(()),
        }
    }
}

fn run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workflow_id": {"type": "string", "minLength": 1},
            "workflow_revision": {"type": "string", "minLength": 1},
            "preset": {"type": "string", "minLength": 1},
            "parameters": {"type": "object"},
            "checkpoint_profile": {"type": "string", "minLength": 1},
            "checkpoint_profile_revision": {"type": "string", "minLength": 1},
            "character": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "spec_version": {"type": "integer", "minimum": 1}
                },
                "required": ["id", "spec_version"],
                "additionalProperties": false
            }
        },
        "required": ["workflow_id", "workflow_revision", "parameters"],
        "additionalProperties": false
    })
}

/// Validates the request as the core will at submission and returns the
/// preview shown to the user. It reads, never writes, and is deterministic for
/// unchanged workflows, packs and characters, because every decision re-runs it.
async fn resolve_run(harness: &Arc<Harness>, arguments: &RunArguments) -> Result<Value> {
    arguments.check_profile_pairing()?;
    let workflow = load_workflow(
        harness,
        &arguments.workflow_id,
        Some(&arguments.workflow_revision),
    )?;
    let layered = layer(harness, &workflow, arguments).await?;
    harness
        .validate_proposal_with_uploads(&workflow, arguments.preset.as_deref(), &layered.overrides)
        .await
        .map_err(proposal_error)?;
    let character = match &arguments.character {
        Some(link) => Some(character_summary(harness, link).await?),
        None => None,
    };
    let preset = arguments.preset.as_deref();
    let bound = workflow.bind(preset, &layered.overrides)?;
    let mut parameters = bound.parameters;
    let mut inherited = Vec::new();
    for (name, definition, value) in workflow.inherited_inputs(preset, &layered.overrides)? {
        parameters
            .entry(name.to_owned())
            .or_insert_with(|| value.clone());
        inherited.push(json!({
            "parameter": name,
            "node": definition.node,
            "input": definition.input,
            "value": value,
            "source": "workflow"
        }));
    }
    let no_profile_values = BTreeMap::new();
    let profile_values = layered
        .profile
        .as_ref()
        .map_or(&no_profile_values, |profile| &profile.values);
    let profile_id = layered.profile.as_ref().map(|profile| profile.id.as_str());
    let effective_values: Vec<Value> = workflow
        .effective_values(preset, profile_values, &arguments.parameters)?
        .into_iter()
        .map(|value| {
            let mut row = json!({
                "parameter": value.parameter,
                "value": value.value,
                "source": value.source
            });
            if value.source == ValueSource::CheckpointProfile {
                row["checkpoint_profile"] = json!(profile_id);
            }
            row
        })
        .collect();
    Ok(json!({
        "workflow_id": workflow.manifest.id,
        "workflow_revision": workflow.revision(),
        "preset": arguments.preset,
        "checkpoint_profile": layered.profile.as_ref().map(|profile| json!({
            "id": profile.id,
            "revision": profile.revision,
            "ignored": profile.ignored
        })),
        "character": character,
        "parameters": parameters,
        "effective_values": effective_values,
        "inherited_inputs": inherited,
        "issues": []
    }))
}

async fn layer(
    harness: &Arc<Harness>,
    workflow: &LoadedWorkflow,
    arguments: &RunArguments,
) -> Result<LayeredBindings> {
    let (harness, workflow, arguments) = (Arc::clone(harness), workflow.clone(), arguments.clone());
    tokio::task::spawn_blocking(move || {
        harness.layer_bindings(
            &workflow,
            arguments.preset.as_deref(),
            arguments.checkpoint_profile.as_deref(),
            arguments.checkpoint_profile_revision.as_deref(),
            &arguments.parameters,
        )
    })
    .await
    .map_err(|error| HarnessError::Workflow(format!("checkpoint profile lookup failed: {error}")))?
}

/// An unknown character or spec version is an error here, before anything is
/// written. Spec versions are append-only, so a verified link stays valid.
async fn character_summary(harness: &Harness, link: &CharacterLink) -> Result<Value> {
    let service = harness.characters();
    let character = service.get_character(&link.id).await?;
    service
        .get_spec_version(&link.id, link.spec_version)
        .await?;
    Ok(json!({
        "id": character.id,
        "name": character.name,
        "spec_version": link.spec_version,
        "latest_spec_version": character.latest_spec_version
    }))
}

struct ProposeRunTool(Arc<Harness>);

#[async_trait]
impl AssistantTool for ProposeRunTool {
    fn name(&self) -> &str {
        "propose_run"
    }

    fn description(&self) -> &str {
        "Validate a pinned run proposal without submitting it. The result shows every effective value with its source (default, checkpoint_profile, preset or user), inherited workflow inputs, the checkpoint profile and revision, and the character link. Pin workflow_revision from inspect_workflow. Name a checkpoint_profile only with its checkpoint_profile_revision, taken from inspect_workflow or list_checkpoints for the exact checkpoint in parameters. Pass character {id, spec_version} only for a spec version you read to complete: true. Never choose image inputs: they are the user's. It never submits."
    }

    fn json_schema(&self) -> Value {
        run_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: RunArguments = serde_json::from_value(arguments)?;
        resolve_run(&self.0, &arguments)
            .await
            .map(bounded_tool_result)
    }
}

struct SubmitRunTool(Arc<Harness>);

#[async_trait]
impl AssistantTool for SubmitRunTool {
    fn name(&self) -> &str {
        "submit_run"
    }

    fn description(&self) -> &str {
        "Submit one exactly reviewed run after the user confirms it. The confirmation card shows every effective value with its source, the pinned workflow revision, the checkpoint profile and its revision, and the character spec version. The workflow revision, profile revision and character link are re-checked at submission, and the confirmation ID is the idempotency key. A result without a run_id is a failure. Pin workflow_revision from inspect_workflow. Name a checkpoint_profile only with its checkpoint_profile_revision, taken from inspect_workflow or list_checkpoints for the exact checkpoint in parameters. Pass character {id, spec_version} only for a spec version you read to complete: true. Never choose image inputs: they are the user's."
    }

    fn json_schema(&self) -> Value {
        run_schema()
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn preview(&self, arguments: &Value) -> Result<Option<Value>> {
        let arguments: RunArguments = serde_json::from_value(arguments.clone())?;
        resolve_run(&self.0, &arguments).await.map(Some)
    }

    async fn invoke(&self, context: ToolContext, arguments: Value) -> Result<Value> {
        let confirmation_id = context.confirmation_id.ok_or_else(|| {
            HarnessError::ToolConfirmationConflict(
                "submit_run requires a confirmation ID and cannot be invoked automatically".into(),
            )
        })?;
        let arguments: RunArguments = serde_json::from_value(arguments)?;
        resolve_run(&self.0, &arguments).await?;
        let submission = self
            .0
            .submit_keyed(confirmation_id, arguments.request())
            .await?;
        submission_result(&arguments, submission)
    }
}

/// A run that was recorded but did not reach the queue is an error that says
/// what was written, never a successful submission.
fn submission_result(arguments: &RunArguments, submission: KeyedSubmission) -> Result<Value> {
    let KeyedSubmission { run, replayed } = submission;
    let advice = match run.status {
        RunStatus::Failed => Some("it failed before it was queued; it will not run"),
        RunStatus::UnknownSubmission => Some(
            "ComfyUI may or may not have accepted it; reconcile it with run_status and never submit it again",
        ),
        RunStatus::Cancelled => Some("it was cancelled"),
        _ => None,
    };
    if let Some(advice) = advice {
        return Err(HarnessError::RunConflict(format!(
            "run '{}' was recorded with status {}: {advice}.{} Do not retry with a new confirmation without asking the user.",
            run.id,
            run.status.as_str(),
            run.error
                .as_deref()
                .map(|error| format!(" {error}"))
                .unwrap_or_default()
        )));
    }
    Ok(bounded_tool_result(json!({
        "run_id": run.id,
        "status": run.status.as_str(),
        "workflow_id": run.workflow_id,
        "workflow_revision": run.workflow_revision(),
        "checkpoint_profile": arguments.checkpoint_profile.as_ref().map(|id| json!({
            "id": id,
            "revision": arguments.checkpoint_profile_revision
        })),
        "character": run.character,
        "replayed": replayed
    })))
}

struct CancelRunTool(Arc<Harness>);

#[async_trait]
impl AssistantTool for CancelRunTool {
    fn name(&self) -> &str {
        "cancel_run"
    }

    fn description(&self) -> &str {
        "Ask ComfyUI to stop one queued or running run, after the user confirms. The confirmation card shows the run, its workflow and its current status. A run that already finished (completed, failed or cancelled) cannot be cancelled and is refused. A successful result means the cancellation was requested and the run is now cancelling; it is cancelled only when run_status says so. If the outcome is unknown, check run_status and never retry blindly."
    }

    fn json_schema(&self) -> Value {
        run_id_schema()
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn preview(&self, arguments: &Value) -> Result<Option<Value>> {
        let RunIdArguments { run_id } = serde_json::from_value(arguments.clone())?;
        let run = self.0.repository().as_async().get(&run_id).await?;
        refuse_uncancellable(&run)?;
        Ok(Some(json!({
            "run_id": run.id,
            "workflow_id": run.workflow_id,
            "workflow_revision": run.workflow_revision(),
            "status": run.status.as_str(),
            "created_at": run.created_at,
            "batch_id": run.batch_id,
            "character": run.character,
            "action": "Ask ComfyUI to stop this run. It is cancelled only when its status says so."
        })))
    }

    async fn invoke(&self, context: ToolContext, arguments: Value) -> Result<Value> {
        if context.confirmation_id.is_none() {
            return Err(HarnessError::ToolConfirmationConflict(
                "cancel_run requires a confirmation ID and cannot be invoked automatically".into(),
            ));
        }
        let RunIdArguments { run_id } = serde_json::from_value(arguments)?;
        let run = self.0.cancel(&run_id).await?;
        let cancelled = run.status == RunStatus::Cancelled;
        Ok(bounded_tool_result(json!({
            "run_id": run.id,
            "workflow_id": run.workflow_id,
            "status": run.status.as_str(),
            "cancellation_requested": true,
            "cancelled": cancelled,
            "note": if cancelled {
                "The run is cancelled."
            } else {
                "ComfyUI was asked to stop this run. It is cancelled only when run_status reports cancelled; do not say it is cancelled before then."
            }
        })))
    }
}

fn refuse_uncancellable(run: &RunRecord) -> Result<()> {
    if run.status.is_terminal() {
        return Err(HarnessError::RunConflict(format!(
            "run '{}' is already {} and cannot be cancelled; nothing was changed",
            run.id,
            run.status.as_str()
        )));
    }
    if run.status == RunStatus::Cancelling {
        return Err(HarnessError::RunConflict(format!(
            "cancellation of run '{}' was already requested; check run_status instead of cancelling again",
            run.id
        )));
    }
    Ok(())
}
