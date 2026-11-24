use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    assistant::VisionReviewService,
    media::{IndexedOutput, OutputListQuery},
    AssistantTool, Harness, HarnessError, LlmRoles, ParameterKind, Result, RunRequest, RunStatus,
    ToolContext,
};

const WORKFLOW_LIST_LIMIT: usize = 50;
const RUN_OUTPUT_LIMIT: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowArguments {
    workflow_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArguments {
    workflow_id: String,
    workflow_revision: String,
    #[serde(default)]
    preset: Option<String>,
    parameters: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunIdArguments {
    run_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputIdArguments {
    output_id: String,
}

pub fn builtin_assistant_tools(
    harness: Arc<Harness>,
    llm_roles: LlmRoles,
) -> Vec<Arc<dyn AssistantTool>> {
    vec![
        Arc::new(ListWorkflowsTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(InspectWorkflowTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(ProposeRunTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(SubmitRunTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(RunStatusTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(GetOutputTool {
            harness: Arc::clone(&harness),
        }),
        Arc::new(ReviewImageTool { harness, llm_roles }),
    ]
}

struct ListWorkflowsTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for ListWorkflowsTool {
    fn name(&self) -> &str {
        "list_workflows"
    }

    fn description(&self) -> &str {
        "List up to 50 valid local workflows and the revision required to inspect or submit one."
    }

    fn json_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, _arguments: Value) -> Result<Value> {
        let discovery = self.harness.discover_workflows()?;
        let mut workflows = discovery.workflows;
        workflows.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        let truncated = workflows.len() > WORKFLOW_LIST_LIMIT;
        workflows.truncate(WORKFLOW_LIST_LIMIT);
        Ok(json!({
            "workflows": workflows.into_iter().map(|workflow| json!({
                "id": workflow.manifest.id,
                "name": workflow.manifest.title,
                "description": workflow.manifest.description,
                "tags": workflow.manifest.tags.unwrap_or_default(),
                "revision": workflow.revision
            })).collect::<Vec<_>>(),
            "truncated": truncated
        }))
    }
}

struct InspectWorkflowTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for InspectWorkflowTool {
    fn name(&self) -> &str {
        "inspect_workflow"
    }

    fn description(&self) -> &str {
        "Inspect one workflow's parameter types, hints, presets and exact revision. Image inputs are explicitly user-supplied."
    }

    fn json_schema(&self) -> Value {
        workflow_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: WorkflowArguments = serde_json::from_value(arguments)?;
        let workflow = load_workflow(&self.harness, &arguments.workflow_id, None)?;
        let parameters = workflow
            .manifest
            .parameters
            .iter()
            .map(|(name, definition)| {
                let parameter = json!({
                    "type": definition.kind,
                    "required": definition.required,
                    "description": definition.description,
                    "default": definition.default,
                    "allowed_values": definition.allowed_values,
                    "minimum": definition.minimum,
                    "maximum": definition.maximum,
                    "widget": definition.widget,
                    "step": definition.step,
                    "options": definition.options,
                    "group": definition.group,
                    "advanced": definition.advanced,
                    "placeholder": definition.placeholder,
                    "user_supplied": definition.kind == ParameterKind::ImageUpload
                });
                (name.clone(), parameter)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(json!({
            "workflow_id": workflow.manifest.id,
            "revision": workflow.revision(),
            "parameters": parameters,
            "presets": workflow.manifest.presets
        }))
    }
}

struct ProposeRunTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for ProposeRunTool {
    fn name(&self) -> &str {
        "propose_run"
    }

    fn description(&self) -> &str {
        "Validate a pinned run proposal and return every effective value, including preset, default and inherited inputs. It never submits."
    }

    fn json_schema(&self) -> Value {
        run_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: RunArguments = serde_json::from_value(arguments)?;
        resolve_run_proposal(&self.harness, arguments).await
    }
}

struct SubmitRunTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for SubmitRunTool {
    fn name(&self) -> &str {
        "submit_run"
    }

    fn description(&self) -> &str {
        "Submit an exactly reviewed, revision-pinned run. The confirmation ID is reused as the idempotency key."
    }

    fn json_schema(&self) -> Value {
        run_schema()
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn preview(&self, arguments: &Value) -> Result<Option<Value>> {
        let arguments: RunArguments = serde_json::from_value(arguments.clone())?;
        resolve_run_proposal(&self.harness, arguments)
            .await
            .map(Some)
    }

    async fn invoke(&self, context: ToolContext, arguments: Value) -> Result<Value> {
        let confirmation_id = context.confirmation_id.ok_or_else(|| {
            HarnessError::ToolConfirmationConflict(
                "submit_run requires a confirmation ID and cannot be invoked automatically".into(),
            )
        })?;
        let arguments: RunArguments = serde_json::from_value(arguments)?;
        resolve_run_request(&self.harness, &arguments).await?;
        let submission = self
            .harness
            .submit_keyed(
                confirmation_id,
                RunRequest {
                    workflow: arguments.workflow_id,
                    workflow_revision: Some(arguments.workflow_revision),
                    preset: arguments.preset,
                    parameters: arguments.parameters,
                    version: None,
                },
            )
            .await?;
        Ok(json!({
            "run_id": submission.run.id,
            "status": submission.run.status.as_str()
        }))
    }
}

struct RunStatusTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for RunStatusTool {
    fn name(&self) -> &str {
        "run_status"
    }

    fn description(&self) -> &str {
        "Reconcile one run and return its durable status. Outputs are included only after the run is completed."
    }

    fn json_schema(&self) -> Value {
        run_id_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: RunIdArguments = serde_json::from_value(arguments)?;
        let run = self.harness.get_run(&arguments.run_id)?;
        let reconciliation = if run.status.is_terminal() {
            None
        } else {
            Some(self.harness.reconcile(&arguments.run_id).await?)
        };
        let run = reconciliation
            .as_ref()
            .map(|result| result.run.clone())
            .unwrap_or(run);
        let outputs = if run.status == RunStatus::Completed {
            self.harness
                .repository()
                .as_async()
                .list_outputs(OutputListQuery {
                    status: Some(RunStatus::Completed),
                    run: Some(run.id.clone()),
                    limit: Some(RUN_OUTPUT_LIMIT),
                    ..Default::default()
                })
                .await?
                .outputs
                .into_iter()
                .map(output_metadata)
                .collect()
        } else {
            Vec::new()
        };
        Ok(json!({
            "run_id": run.id,
            "status": run.status.as_str(),
            "outputs": outputs,
            "error": run.error,
            "reconciliation": reconciliation.map(|result| json!({
                "outcome": result.outcome,
                "settles_at": result.settles_at
            }))
        }))
    }
}

struct GetOutputTool {
    harness: Arc<Harness>,
}

#[async_trait]
impl AssistantTool for GetOutputTool {
    fn name(&self) -> &str {
        "get_output"
    }

    fn description(&self) -> &str {
        "Return indexed output metadata only. Original bytes are never returned."
    }

    fn json_schema(&self) -> Value {
        output_id_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: OutputIdArguments = serde_json::from_value(arguments)?;
        let output = self
            .harness
            .repository()
            .as_async()
            .output(&arguments.output_id)
            .await?;
        Ok(output_metadata(output))
    }
}

struct ReviewImageTool {
    harness: Arc<Harness>,
    llm_roles: LlmRoles,
}

#[async_trait]
impl AssistantTool for ReviewImageTool {
    fn name(&self) -> &str {
        "review_image"
    }

    fn description(&self) -> &str {
        "Review an output with the exact configured vision model and append a model-opinion annotation. This does not submit a generation run."
    }

    fn json_schema(&self) -> Value {
        output_id_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: OutputIdArguments = serde_json::from_value(arguments)?;
        let binding = self.llm_roles.vision().await.map_err(|error| {
            HarnessError::VisionReviewUnavailable {
                reason: error.to_string(),
            }
        })?;
        let annotation = VisionReviewService::new(self.harness.as_ref().clone())
            .review_output(&binding, &arguments.output_id)
            .await?;
        Ok(serde_json::to_value(annotation)?)
    }
}

async fn resolve_run_proposal(harness: &Harness, arguments: RunArguments) -> Result<Value> {
    let workflow = load_workflow(
        harness,
        &arguments.workflow_id,
        Some(&arguments.workflow_revision),
    )?;
    harness
        .validate_proposal_with_uploads(
            &workflow,
            arguments.preset.as_deref(),
            &arguments.parameters,
        )
        .await
        .map_err(proposal_error)?;
    let bound = workflow.bind(arguments.preset.as_deref(), &arguments.parameters)?;
    let mut effective_parameters = bound.parameters;
    for (name, _, value) in
        workflow.inherited_inputs(arguments.preset.as_deref(), &arguments.parameters)?
    {
        effective_parameters
            .entry(name.to_owned())
            .or_insert_with(|| value.clone());
    }
    Ok(json!({
        "workflow_id": workflow.manifest.id,
        "workflow_revision": workflow.revision(),
        "preset": arguments.preset,
        "parameters": effective_parameters,
        "issues": []
    }))
}

async fn resolve_run_request(harness: &Harness, arguments: &RunArguments) -> Result<()> {
    let workflow = load_workflow(
        harness,
        &arguments.workflow_id,
        Some(&arguments.workflow_revision),
    )?;
    harness
        .validate_proposal_with_uploads(
            &workflow,
            arguments.preset.as_deref(),
            &arguments.parameters,
        )
        .await
        .map_err(proposal_error)?;
    Ok(())
}

fn load_workflow(
    harness: &Harness,
    workflow_id: &str,
    revision: Option<&str>,
) -> Result<crate::LoadedWorkflow> {
    harness
        .load_api_workflow(workflow_id, revision)?
        .ok_or_else(|| HarnessError::Workflow(format!("workflow '{workflow_id}' was not found")))
}

fn proposal_error(issues: Vec<crate::assistant::suggest::ProposalIssue>) -> HarnessError {
    HarnessError::InvalidToolArguments(
        issues
            .into_iter()
            .map(|issue| format!("{}: {}", issue.field, issue.reason))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn output_metadata(output: IndexedOutput) -> Value {
    json!({
        "output_id": output.id,
        "run_id": output.output.run_id,
        "workflow_id": output.workflow_id,
        "node_id": output.output.node_id,
        "kind": output.output.kind,
        "source_filename": output.output.source_filename,
        "sha256": output.output.sha256,
        "byte_size": output.output.byte_size,
        "created_at": output.output.created_at,
        "tags": output.tags
    })
}

fn workflow_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workflow_id": {"type": "string", "minLength": 1}
        },
        "required": ["workflow_id"],
        "additionalProperties": false
    })
}

fn run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workflow_id": {"type": "string", "minLength": 1},
            "workflow_revision": {"type": "string", "minLength": 1},
            "preset": {"type": "string", "minLength": 1},
            "parameters": {"type": "object"}
        },
        "required": ["workflow_id", "workflow_revision", "parameters"],
        "additionalProperties": false
    })
}

fn run_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "run_id": {"type": "string", "minLength": 1}
        },
        "required": ["run_id"],
        "additionalProperties": false
    })
}

fn output_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "output_id": {"type": "string", "minLength": 1}
        },
        "required": ["output_id"],
        "additionalProperties": false
    })
}
