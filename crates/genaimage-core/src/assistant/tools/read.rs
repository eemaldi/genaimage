//! Read-only assistant tools: what the user could see in the portal.
//!
//! None of these tools writes, submits or uploads. Every list is bounded and
//! says when it was truncated; an unreachable service is an error, never an
//! empty list.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{bounded_tool_result, output_id_schema, output_metadata};
use crate::{
    characters::{CharacterListQuery, Decision},
    characters::{CharacterRunQuery, CurrentStateQuery, ReferenceListQuery},
    media::OutputListQuery,
    persistence::RunListQuery,
    AssistantTool, Harness, HarnessError, Result, RunRecord, RunStatus, ToolContext,
};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
const MAX_ANNOTATIONS: usize = 10;
const RECENT_APPROVED: usize = 5;
const COUNT_CAP: usize = 200;
const OUTPUT_COUNT_CAP: usize = 50;
const COMFYUI_DEADLINE: Duration = Duration::from_secs(10);

pub const PROFILE_NOTE: &str = "Profiles match a checkpoint only by its exact name. A checkpoint with an empty list has no profile; no model family is inferred from file names, and a profile is applied only when a request names it together with its revision.";

pub(super) fn read_tools(harness: &Arc<Harness>) -> Vec<Arc<dyn AssistantTool>> {
    vec![
        Arc::new(ListCheckpointsTool(Arc::clone(harness))),
        Arc::new(ListRunsTool(Arc::clone(harness))),
        Arc::new(ListOutputsTool(Arc::clone(harness))),
        Arc::new(ListCharactersTool(Arc::clone(harness))),
        Arc::new(GetCharacterTool(Arc::clone(harness))),
        Arc::new(ListAnnotationsTool(Arc::clone(harness))),
    ]
}

fn page_schema(extra: Value) -> Value {
    let mut properties = json!({
        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT}
    });
    if let (Some(properties), Value::Object(extra)) = (properties.as_object_mut(), extra) {
        properties.extend(extra);
    }
    json!({"type": "object", "properties": properties, "required": [], "additionalProperties": false})
}

fn limit_of(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(HarnessError::InvalidToolArguments(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(limit)
}

fn paged(key: &str, items: Vec<Value>, next_cursor: Option<String>, limit: usize) -> Value {
    let mut result = json!({
        key: items,
        "limit": limit,
        "truncated": next_cursor.is_some(),
        "next_cursor": next_cursor
    });
    if result["truncated"] == true {
        result["truncation_note"] = json!(
            "More results exist. Pass next_cursor as cursor to read the next page; this page is not the complete list."
        );
    }
    result
}

fn invalid(message: &str) -> HarnessError {
    HarnessError::InvalidToolArguments(message.into())
}

struct ListCheckpointsTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointArguments {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[async_trait]
impl AssistantTool for ListCheckpointsTool {
    fn name(&self) -> &str {
        "list_checkpoints"
    }

    fn description(&self) -> &str {
        "List checkpoints installed in ComfyUI (sorted, up to 50 per page) with the checkpoint profiles whose exact checkpoint names match. If ComfyUI is unreachable this is an error, not an empty list. A profile is applied only when a run names its id and revision."
    }

    fn json_schema(&self) -> Value {
        page_schema(json!({}))
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: CheckpointArguments = serde_json::from_value(arguments)?;
        let limit = limit_of(arguments.limit)?;
        let names = tokio::time::timeout(COMFYUI_DEADLINE, crate::models::list_checkpoints(&self.0))
            .await
            .map_err(|_| {
                HarnessError::ComfyUi(
                    "ComfyUI did not list its checkpoints in time. This is not an empty list; check that ComfyUI is running.".into(),
                )
            })??;
        let harness = Arc::clone(&self.0);
        let packs = tokio::task::spawn_blocking(move || harness.discover_packs())
            .await
            .map_err(|error| HarnessError::Configuration(error.to_string()))?;
        let mut remaining = names.iter().filter(|name| {
            arguments
                .cursor
                .as_ref()
                .is_none_or(|cursor| *name > cursor)
        });
        let page: Vec<&String> = remaining.by_ref().take(limit).collect();
        let more = remaining.next().is_some();
        let checkpoints = page
            .iter()
            .map(|name| {
                let profiles = packs
                    .packs
                    .iter()
                    .flat_map(|pack| {
                        pack.checkpoint_profiles
                            .iter()
                            .filter(|profile| profile.matches(name))
                            .map(|profile| {
                                json!({
                                    "pack": pack.manifest.id,
                                    "id": profile.id,
                                    "label": profile.label,
                                    "revision": profile.revision()
                                })
                            })
                    })
                    .collect::<Vec<_>>();
                json!({"name": name, "profiles": profiles})
            })
            .collect();
        let mut result = paged(
            "checkpoints",
            checkpoints,
            more.then(|| page.last().map(|name| (*name).clone()))
                .flatten(),
            limit,
        );
        result["profile_note"] = json!(PROFILE_NOTE);
        Ok(bounded_tool_result(result))
    }
}

struct ListRunsTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunsArguments {
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    status: Option<RunStatus>,
    #[serde(default)]
    character_id: Option<String>,
    #[serde(default)]
    spec_version: Option<u32>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[async_trait]
impl AssistantTool for ListRunsTool {
    fn name(&self) -> &str {
        "list_runs"
    }

    fn description(&self) -> &str {
        "List recent runs, newest first (default 20, at most 50 per page), optionally filtered by workflow, status or character (with spec_version). The character filter cannot be combined with the workflow filter. Rows carry no parameters; use run_status for one run."
    }

    fn json_schema(&self) -> Value {
        page_schema(json!({
            "workflow": {"type": "string", "minLength": 1},
            "status": {"type": "string", "enum": [
                "created", "submitting", "unknown_submission", "queued", "running",
                "completed", "failed", "cancelling", "cancelled"
            ]},
            "character_id": {"type": "string", "minLength": 1},
            "spec_version": {"type": "integer", "minimum": 1}
        }))
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: RunsArguments = serde_json::from_value(arguments)?;
        let limit = limit_of(arguments.limit)?;
        let page = match (
            &arguments.character_id,
            &arguments.workflow,
            arguments.spec_version,
        ) {
            (Some(_), Some(_), _) => {
                return Err(invalid(
                    "character_id cannot be combined with workflow; filter by one of them",
                ))
            }
            (None, _, Some(_)) => return Err(invalid("spec_version requires character_id")),
            (Some(character_id), None, spec_version) => {
                self.0
                    .characters()
                    .list_runs(
                        character_id,
                        CharacterRunQuery {
                            spec_version,
                            status: arguments.status,
                            limit: Some(limit),
                            cursor: arguments.cursor,
                        },
                    )
                    .await?
            }
            (None, workflow, None) => {
                self.0
                    .repository()
                    .as_async()
                    .list_page(RunListQuery {
                        status: arguments.status,
                        workflow: workflow.clone(),
                        limit: Some(limit),
                        cursor: arguments.cursor,
                        ..Default::default()
                    })
                    .await?
            }
        };
        let mut rows = Vec::with_capacity(page.runs.len());
        for run in &page.runs {
            rows.push(self.row(run).await?);
        }
        Ok(bounded_tool_result(paged(
            "runs",
            rows,
            page.next_cursor,
            limit,
        )))
    }
}

impl ListRunsTool {
    async fn row(&self, run: &RunRecord) -> Result<Value> {
        let outputs = self
            .0
            .repository()
            .as_async()
            .list_outputs(OutputListQuery {
                run: Some(run.id.clone()),
                limit: Some(OUTPUT_COUNT_CAP),
                ..Default::default()
            })
            .await?;
        Ok(json!({
            "id": run.id,
            "status": run.status.as_str(),
            "workflow_id": run.workflow_id,
            "workflow_revision": run.workflow_revision(),
            "created_at": run.created_at,
            "completed_at": run.completed_at,
            "character": run.character,
            "output_count": outputs.outputs.len(),
            "output_count_capped": outputs.next_cursor.is_some(),
            "error": run.error
        }))
    }
}

struct ListOutputsTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputsArguments {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    character_id: Option<String>,
    #[serde(default)]
    spec_version: Option<u32>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[async_trait]
impl AssistantTool for ListOutputsTool {
    fn name(&self) -> &str {
        "list_outputs"
    }

    fn description(&self) -> &str {
        "List indexed output metadata, newest first (default 20, at most 50 per page), for one run or one character (with spec_version), or all outputs. Metadata only; image bytes are never returned. Filters cannot be combined."
    }

    fn json_schema(&self) -> Value {
        page_schema(json!({
            "run_id": {"type": "string", "minLength": 1},
            "character_id": {"type": "string", "minLength": 1},
            "spec_version": {"type": "integer", "minimum": 1}
        }))
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: OutputsArguments = serde_json::from_value(arguments)?;
        let limit = limit_of(arguments.limit)?;
        let page = match (
            &arguments.run_id,
            &arguments.character_id,
            arguments.spec_version,
        ) {
            (Some(_), Some(_), _) => {
                return Err(invalid("run_id cannot be combined with character_id"))
            }
            (_, None, Some(_)) => return Err(invalid("spec_version requires character_id")),
            (None, Some(character_id), spec_version) => {
                self.0
                    .characters()
                    .list_outputs(
                        character_id,
                        CharacterRunQuery {
                            spec_version,
                            status: None,
                            limit: Some(limit),
                            cursor: arguments.cursor,
                        },
                    )
                    .await?
            }
            (run, None, None) => {
                self.0
                    .repository()
                    .as_async()
                    .list_outputs(OutputListQuery {
                        run: run.clone(),
                        limit: Some(limit),
                        cursor: arguments.cursor,
                        ..Default::default()
                    })
                    .await?
            }
        };
        Ok(bounded_tool_result(paged(
            "outputs",
            page.outputs.into_iter().map(output_metadata).collect(),
            page.next_cursor,
            limit,
        )))
    }
}

struct ListCharactersTool(Arc<Harness>);

#[async_trait]
impl AssistantTool for ListCharactersTool {
    fn name(&self) -> &str {
        "list_characters"
    }

    fn description(&self) -> &str {
        "List characters, newest first (default 20, at most 50 per page), with their latest spec version number."
    }

    fn json_schema(&self) -> Value {
        page_schema(json!({}))
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let arguments: CheckpointArguments = serde_json::from_value(arguments)?;
        let limit = limit_of(arguments.limit)?;
        let page = self
            .0
            .characters()
            .list_characters(CharacterListQuery {
                limit: Some(limit),
                cursor: arguments.cursor,
            })
            .await?;
        let characters = page
            .characters
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(bounded_tool_result(paged(
            "characters",
            characters,
            page.next_cursor,
            limit,
        )))
    }
}

struct GetCharacterTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CharacterArguments {
    character_id: String,
}

#[async_trait]
impl AssistantTool for GetCharacterTool {
    fn name(&self) -> &str {
        "get_character"
    }

    fn description(&self) -> &str {
        "Return one character with its latest spec version (full spec), its active reference images (informational only; the user chooses image inputs) and a summary of current decisions: counts and the most recent approved outputs."
    }

    fn json_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"character_id": {"type": "string", "minLength": 1}},
            "required": ["character_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let CharacterArguments { character_id } = serde_json::from_value(arguments)?;
        let service = self.0.characters();
        let character = service.get_character(&character_id).await?;
        let spec = service.latest_spec_version(&character_id).await?;
        let references = service
            .list_references(
                &character_id,
                ReferenceListQuery {
                    include_removed: false,
                    limit: Some(crate::characters::MAX_ACTIVE_REFERENCES),
                    cursor: None,
                },
            )
            .await?;
        let count = |state| {
            let service = service.clone();
            let character_id = character_id.clone();
            async move {
                let page = service
                    .list_current_states(
                        &character_id,
                        CurrentStateQuery {
                            state: Some(state),
                            limit: Some(COUNT_CAP),
                            ..Default::default()
                        },
                    )
                    .await?;
                Ok::<_, HarnessError>(json!({
                    "count": page.decisions.len(),
                    "count_is_lower_bound": page.next_cursor.is_some()
                }))
            }
        };
        let approved = count(Decision::Approved).await?;
        let rejected = count(Decision::Rejected).await?;
        let recent = service
            .list_approved_with_outputs(
                &character_id,
                CurrentStateQuery {
                    state: Some(Decision::Approved),
                    limit: Some(RECENT_APPROVED),
                    ..Default::default()
                },
            )
            .await?;
        Ok(bounded_tool_result(json!({
            "character": character,
            "latest_spec_version": spec,
            "references": {
                "items": references.references,
                "truncated": references.next_cursor.is_some(),
                "note": "Informational only. Image inputs are always chosen by the user for each request."
            },
            "decisions": {
                "approved": approved,
                "rejected": rejected,
                "recent_approved": recent.decisions.into_iter().map(|row| json!({
                    "decision_id": row.decision.id,
                    "output_id": row.decision.output_id,
                    "spec_version": row.decision.spec_version,
                    "decided_at": row.decision.decided_at,
                    "note": row.decision.note,
                    "output": output_metadata(row.output)
                })).collect::<Vec<_>>(),
                "recent_approved_limit": RECENT_APPROVED,
                "note": "Decisions are the user's; a successful run is not approval."
            }
        })))
    }
}

struct ListAnnotationsTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnnotationArguments {
    output_id: String,
}

#[async_trait]
impl AssistantTool for ListAnnotationsTool {
    fn name(&self) -> &str {
        "list_annotations"
    }

    fn description(&self) -> &str {
        "List recorded annotations for an output, newest first (at most 10). They are vision-model opinions, not facts or user approval. Prompts and raw model responses are omitted."
    }

    fn json_schema(&self) -> Value {
        output_id_schema()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let AnnotationArguments { output_id } = serde_json::from_value(arguments)?;
        let repository = self.0.repository().as_async();
        repository.output(&output_id).await?;
        let annotations = repository.output_annotations(&output_id).await?;
        let truncated = annotations.len() > MAX_ANNOTATIONS;
        Ok(bounded_tool_result(json!({
            "output_id": output_id,
            "source": "model_opinion",
            "note": "These are model opinions recorded by the vision role, not provenance and not user approval.",
            "annotations": annotations.into_iter().take(MAX_ANNOTATIONS).map(|annotation| json!({
                "id": annotation.id,
                "kind": annotation.kind,
                "model": annotation.model,
                "created_at": annotation.created_at,
                "caption": annotation.caption,
                "adherence_critique": annotation.adherence_critique,
                "tags": annotation.tags
            })).collect::<Vec<_>>(),
            "limit": MAX_ANNOTATIONS,
            "truncated": truncated,
            "truncation_note": truncated.then_some("Older annotations exist and are not shown.")
        })))
    }
}
