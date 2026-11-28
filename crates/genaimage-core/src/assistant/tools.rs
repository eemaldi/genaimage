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

mod read;

const WORKFLOW_LIST_LIMIT: usize = 50;
const RUN_OUTPUT_LIMIT: usize = 50;
const MAX_TOOL_RESULT_BYTES: usize = 32 * 1024;
const MAX_TOOL_WORKING_BYTES: usize = 28 * 1024;
const MAX_TOOL_STRING_BYTES: usize = 2 * 1024;
const MAX_TOOL_ARRAY_ITEMS: usize = 50;
const MAX_TOOL_OBJECT_FIELDS: usize = 100;
const MAX_TOOL_NOTES: usize = 64;

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
    let mut tools: Vec<Arc<dyn AssistantTool>> = vec![
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
        Arc::new(ReviewAgainstSpecTool {
            harness: Arc::clone(&harness),
            llm_roles: llm_roles.clone(),
        }),
        Arc::new(ReviewImageTool {
            harness: Arc::clone(&harness),
            llm_roles,
        }),
    ];
    tools.extend(read::read_tools(&harness));
    tools
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
        workflows.retain(|workflow| workflow.unavailable_reason.is_none());
        workflows.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        let truncated = workflows.len() > WORKFLOW_LIST_LIMIT;
        workflows.truncate(WORKFLOW_LIST_LIMIT);
        Ok(bounded_tool_result(json!({
            "workflows": workflows.into_iter().map(|workflow| json!({
                "id": workflow.manifest.id,
                "name": workflow.manifest.title,
                "description": workflow.manifest.description,
                "tags": workflow.manifest.tags.unwrap_or_default(),
                "revision": workflow.revision
            })).collect::<Vec<_>>(),
            "truncated": truncated
        })))
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
        "Inspect one workflow's parameter types, hints, presets (with preset_info labels, descriptions and kinds such as layout, style or quality), exact revision, checkpoint parameter and checkpoint profiles (id, label, checkpoints, revision, ignored). When profile_pack_problems is present, pack files failed to load and an empty profile list does not mean there is no profile. Presets and profiles apply only when a run names them; a profile must be named with its revision. Image inputs are explicitly user-supplied."
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
        let profiled = workflow.clone();
        let harness = Arc::clone(&self.harness);
        let lookup = tokio::task::spawn_blocking(move || harness.pack_lookup_of(&profiled))
            .await
            .map_err(|error| HarnessError::Workflow(error.to_string()))?;
        let checkpoint_profiles = lookup
            .pack
            .map(|pack| pack.checkpoint_profiles)
            .unwrap_or_default()
            .into_iter()
            .map(|profile| {
                json!({
                    "id": profile.id,
                    "label": profile.label,
                    "checkpoints": profile.checkpoints,
                    "note": profile.note,
                    "values": profile.values_for(&workflow.manifest),
                    "ignored": profile.ignored_for(&workflow.manifest),
                    "revision": profile.revision()
                })
            })
            .collect::<Vec<_>>();
        let mut result = json!({
            "workflow_id": workflow.manifest.id,
            "revision": workflow.revision(),
            "parameters": parameters,
            "presets": workflow.manifest.presets,
            "preset_info": workflow.manifest.preset_info,
            "checkpoint_parameter": workflow.checkpoint_parameter(),
            "checkpoint_profiles": checkpoint_profiles
        });
        let problems = read::pack_problems(&lookup.problems);
        if !checkpoint_profiles.is_empty() || problems.is_some() {
            result["profile_note"] = json!(read::PROFILE_NOTE);
        }
        if let Some(problems) = problems {
            result["profile_pack_problems"] = problems;
        }
        Ok(bounded_tool_result(result))
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
        resolve_run_proposal(&self.harness, arguments)
            .await
            .map(bounded_tool_result)
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
                    character: None,
                    preset: arguments.preset,
                    parameters: arguments.parameters,
                    version: None,
                    checkpoint_profile: None,
                    checkpoint_profile_revision: None,
                },
            )
            .await?;
        Ok(bounded_tool_result(json!({
            "run_id": submission.run.id,
            "status": submission.run.status.as_str()
        })))
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
        Ok(bounded_tool_result(json!({
            "run_id": run.id,
            "status": run.status.as_str(),
            "outputs": outputs,
            "error": run.error,
            "reconciliation": reconciliation.map(|result| json!({
                "outcome": result.outcome,
                "settles_at": result.settles_at
            }))
        })))
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
        Ok(bounded_tool_result(output_metadata(output)))
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
        Ok(bounded_tool_result(serde_json::to_value(annotation)?))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecReviewArguments {
    output_id: String,
    character_id: String,
    spec_version: u32,
}

struct ReviewAgainstSpecTool {
    harness: Arc<Harness>,
    llm_roles: LlmRoles,
}

#[async_trait]
impl AssistantTool for ReviewAgainstSpecTool {
    fn name(&self) -> &str {
        "review_against_spec"
    }

    fn description(&self) -> &str {
        "Review an output against one exact character spec version with the configured vision model and append a model-opinion spec_review annotation. It is not an approval or rejection and does not submit a generation run."
    }

    fn json_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output_id": {"type": "string", "minLength": 1},
                "character_id": {"type": "string", "minLength": 1},
                "spec_version": {"type": "integer", "minimum": 1}
            },
            "required": ["output_id", "character_id", "spec_version"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: SpecReviewArguments = serde_json::from_value(arguments)?;
        let binding = self.llm_roles.vision().await.map_err(|error| {
            HarnessError::VisionReviewUnavailable {
                reason: error.to_string(),
            }
        })?;
        let annotation = VisionReviewService::new(self.harness.as_ref().clone())
            .review_against_spec(
                &binding,
                &arguments.output_id,
                &arguments.character_id,
                arguments.spec_version,
            )
            .await?;
        Ok(bounded_tool_result(serde_json::to_value(annotation)?))
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

fn bounded_tool_result(value: Value) -> Value {
    let mut notes = Vec::new();
    let mut remaining = MAX_TOOL_WORKING_BYTES;
    let bounded =
        bound_tool_value(value, "$", 0, &mut remaining, &mut notes).unwrap_or(Value::Null);
    let mut result = match bounded {
        Value::Object(values) => Value::Object(values),
        other => json!({"result": other}),
    };
    if !notes.is_empty() {
        let object = result.as_object_mut().expect("tool results are objects");
        object.insert("truncated".into(), Value::Bool(true));
        object.insert(
            "truncation_notes".into(),
            Value::Array(
                notes
                    .into_iter()
                    .take(MAX_TOOL_NOTES)
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    while serde_json::to_vec(&result)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
        > MAX_TOOL_RESULT_BYTES
    {
        let Some(object) = result.as_object_mut() else {
            return json!({
                "truncated": true,
                "truncation_notes": ["The complete result could not fit within 32768 bytes."]
            });
        };
        let removable = object
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "truncated" | "truncation_notes"))
            .max_by_key(|(_, value)| {
                serde_json::to_vec(value)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0)
            })
            .map(|(name, _)| name.clone());
        let Some(removable) = removable else {
            return json!({
                "truncated": true,
                "truncation_notes": ["The complete result could not fit within 32768 bytes."]
            });
        };
        object.remove(&removable);
        object.insert("truncated".into(), Value::Bool(true));
        let notes = object
            .entry("truncation_notes")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(notes) = notes {
            if notes.len() < MAX_TOOL_NOTES {
                notes.push(Value::String(format!(
                    "Omitted $.{removable} because the complete tool result exceeded 32768 bytes."
                )));
            }
        }
    }
    result
}

fn bound_tool_value(
    value: Value,
    path: &str,
    depth: usize,
    remaining: &mut usize,
    notes: &mut Vec<String>,
) -> Option<Value> {
    if depth > 16 {
        note(notes, format!("Omitted {path}: nesting limit reached."));
        return None;
    }
    match value {
        Value::String(text) => bound_tool_string(text, path, remaining, notes),
        Value::Array(values) => {
            let truncated = values.len() > MAX_TOOL_ARRAY_ITEMS;
            let mut bounded = Vec::new();
            for (index, value) in values.into_iter().take(MAX_TOOL_ARRAY_ITEMS).enumerate() {
                let child_path = format!("{path}[{index}]");
                match bound_tool_value(value, &child_path, depth + 1, remaining, notes) {
                    Some(value) => bounded.push(value),
                    None => note(
                        notes,
                        format!("Omitted {child_path} without truncating its value."),
                    ),
                }
            }
            if truncated {
                note(
                    notes,
                    format!("Truncated {path} to the first {MAX_TOOL_ARRAY_ITEMS} array entries."),
                );
            }
            Some(Value::Array(bounded))
        }
        Value::Object(values) => {
            let mut bounded = serde_json::Map::new();
            let field_count = values.len();
            for (name, value) in values.into_iter().take(MAX_TOOL_OBJECT_FIELDS) {
                let child_path = format!("{path}.{name}");
                if let Some(value) =
                    bound_tool_value(value, &child_path, depth + 1, remaining, notes)
                {
                    bounded.insert(name, value);
                } else {
                    note(
                        notes,
                        format!(
                            "Omitted {child_path}; numbers and identifiers are never shortened."
                        ),
                    );
                }
            }
            if field_count > MAX_TOOL_OBJECT_FIELDS {
                note(
                    notes,
                    format!(
                        "Truncated {path} to the first {MAX_TOOL_OBJECT_FIELDS} object fields."
                    ),
                );
            }
            Some(Value::Object(bounded))
        }
        other => reserve_value(other, path, remaining, notes),
    }
}

fn bound_tool_string(
    text: String,
    path: &str,
    remaining: &mut usize,
    notes: &mut Vec<String>,
) -> Option<Value> {
    let oversized = text.len() > MAX_TOOL_STRING_BYTES;
    if oversized && is_identifier_path(path) {
        note(
            notes,
            format!("Omitted {path}: an oversized identifier was not shortened."),
        );
        return None;
    }
    let value = if oversized {
        let mut end = MAX_TOOL_STRING_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        note(
            notes,
            format!("Truncated {path} to {MAX_TOOL_STRING_BYTES} UTF-8 bytes."),
        );
        format!("{}…", &text[..end])
    } else {
        text
    };
    reserve_value(Value::String(value), path, remaining, notes)
}

fn reserve_value(
    value: Value,
    path: &str,
    remaining: &mut usize,
    notes: &mut Vec<String>,
) -> Option<Value> {
    let size = serde_json::to_vec(&value).map(|bytes| bytes.len()).ok()?;
    if size > *remaining {
        note(notes, format!("Omitted {path} because it exceeded the 32768-byte result budget; its value was not shortened."));
        return None;
    }
    *remaining -= size;
    Some(value)
}

fn is_identifier_path(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|name| {
        matches!(name, "id" | "revision" | "sha256" | "source_filename")
            || name.ends_with("_id")
            || name.ends_with("_revision")
    })
}

fn note(notes: &mut Vec<String>, message: String) {
    if notes.len() < MAX_TOOL_NOTES {
        notes.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_results_truncate_text_and_arrays_without_changing_exact_values() {
        let large = "x".repeat(MAX_TOOL_STRING_BYTES + 200);
        let result = bounded_tool_result(json!({
            "workflow_id": "workflow-exact",
            "description": large,
            "seed": 9007199254740993_u64,
            "items": (0..60).map(|index| json!({"index": index})).collect::<Vec<_>>(),
            "oversized_id": "y".repeat(5000),
            "payload": "z".repeat(MAX_TOOL_RESULT_BYTES * 2)
        }));

        let bytes = serde_json::to_vec(&result).unwrap();
        assert!(
            bytes.len() <= MAX_TOOL_RESULT_BYTES,
            "{} bytes",
            bytes.len()
        );
        assert_eq!(result["workflow_id"], "workflow-exact");
        assert_eq!(result["seed"], json!(9007199254740993_u64));
        assert_eq!(
            result["items"].as_array().unwrap().len(),
            MAX_TOOL_ARRAY_ITEMS
        );
        assert_eq!(result["truncated"], true);
        assert!(result.get("oversized_id").is_none());
        assert!(result["description"].as_str().unwrap().ends_with('…'));
        assert!(result["payload"].as_str().unwrap().ends_with('…'));
        assert!(result["truncation_notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("items")));
    }

    #[test]
    fn oversized_identifiers_are_omitted_with_an_explicit_note() {
        let result = bounded_tool_result(json!({
            "run_id": "r".repeat(MAX_TOOL_STRING_BYTES + 1),
            "status": "queued"
        }));
        assert!(result.get("run_id").is_none());
        assert_eq!(result["status"], "queued");
        assert!(result["truncation_notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("identifier")));
    }
}
