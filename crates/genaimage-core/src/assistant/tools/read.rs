//! Read-only assistant tools: what the user could see in the portal.
//!
//! None of these tools writes, submits or uploads. Every list is bounded and
//! says when it was truncated; an unreachable service is an error, never an
//! empty list.

use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    bounded_tool_result, output_id_schema, output_metadata, MAX_TOOL_RESULT_BYTES,
    MAX_TOOL_STRING_BYTES,
};
use crate::{
    characters::{CharacterListQuery, Decision},
    characters::{CharacterRunQuery, CurrentStateQuery, ReferenceListQuery},
    characters::{CharacterSpec, CharacterSpecVersion},
    media::OutputListQuery,
    persistence::{RunListQuery, OUTPUT_ANNOTATION_LIMIT},
    AssistantTool, Harness, HarnessError, Result, RunRecord, RunStatus, ToolContext,
};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
const MAX_ANNOTATIONS: usize = 10;
const RECENT_APPROVED: usize = 5;
const COUNT_CAP: usize = 200;
const OUTPUT_COUNT_CAP: usize = 50;
/// Row and element bytes a page may carry; the envelope and notes fit below
/// the 32 KiB tool-result cap.
const PAGE_BUDGET_BYTES: usize = 24 * 1024;
const ERROR_CAP_BYTES: usize = 300;
const ANNOTATION_TEXT_CAP_BYTES: usize = 1_500;
const MAX_PROBLEM_MESSAGES: usize = 5;
const SHORTENED_NOTE: &str = "Page shortened to fit; request the next page with next_cursor. This page is not the complete list.";
const COMFYUI_DEADLINE: Duration = Duration::from_secs(10);

pub const PROFILE_NOTE: &str = "Profiles match a checkpoint only by its exact name; no model family is inferred from file names, and a profile is applied only when a request names it together with its revision. An empty profile list means no profile was loaded. When profile_pack_problems is present, pack files failed to load, so an empty list may mean a failed load and not that no profile exists.";

pub(super) fn read_tools(harness: &Arc<Harness>) -> Vec<Arc<dyn AssistantTool>> {
    vec![
        Arc::new(ListCheckpointsTool(Arc::clone(harness))),
        Arc::new(ListRunsTool(Arc::clone(harness))),
        Arc::new(ListOutputsTool(Arc::clone(harness))),
        Arc::new(ListCharactersTool(Arc::clone(harness))),
        Arc::new(GetCharacterTool(Arc::clone(harness))),
        Arc::new(GetCharacterSpecTool(Arc::clone(harness))),
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

fn paged(
    key: &str,
    items: Vec<Value>,
    next_cursor: Option<String>,
    limit: usize,
    shortened: bool,
) -> Value {
    let mut result = json!({
        key: items,
        "limit": limit,
        "truncated": next_cursor.is_some(),
        "next_cursor": next_cursor
    });
    if result["truncated"] == true {
        result["truncation_note"] = json!(if shortened {
            SHORTENED_NOTE
        } else {
            "More results exist. Pass next_cursor as cursor to read the next page; this page is not the complete list."
        });
    }
    result
}

fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// Shortens `text` to `max` UTF-8 bytes and says so; never splits a character.
fn cap_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncated)", &text[..end])
}

/// Pages rows under the page budget without ever hollowing a row. `fetch(n)`
/// returns up to `n` rows after the request's cursor and the cursor of its
/// last row when more exist. When the rows do not fit, the same page is
/// fetched again with the largest fitting `n`, so `next_cursor` is the
/// repository's own cursor for the last included row.
async fn budgeted_page<F, Fut>(key: &str, limit: usize, fetch: F) -> Result<Value>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Result<(Vec<Value>, Option<String>)>>,
{
    let (rows, next_cursor) = fetch(limit).await?;
    let mut used = 0;
    let fitting = rows
        .iter()
        .take_while(|row| {
            used += json_size(row) + 1;
            used <= PAGE_BUDGET_BYTES
        })
        .count();
    if fitting == rows.len() {
        return Ok(bounded_tool_result(paged(
            key,
            rows,
            next_cursor,
            limit,
            false,
        )));
    }
    if fitting == 0 {
        return Err(HarnessError::InvalidToolArguments(format!(
            "A single {key} row exceeds the {PAGE_BUDGET_BYTES}-byte page budget and cannot be returned whole."
        )));
    }
    let (rows, next_cursor) = fetch(fitting).await?;
    if rows.len() != fitting || next_cursor.is_none() {
        return Err(HarnessError::InvalidToolArguments(format!(
            "The {key} list changed while its page was being shortened; request it again."
        )));
    }
    Ok(bounded_tool_result(paged(
        key,
        rows,
        next_cursor,
        limit,
        true,
    )))
}

/// The first pack discovery problems, or `None` when there were none.
pub(super) fn pack_problems(problems: &[String]) -> Option<Value> {
    (!problems.is_empty()).then(|| {
        json!({
            "count": problems.len(),
            "messages": problems
                .iter()
                .take(MAX_PROBLEM_MESSAGES)
                .map(|problem| cap_text(problem, ERROR_CAP_BYTES))
                .collect::<Vec<_>>(),
            "shown": problems.len().min(MAX_PROBLEM_MESSAGES)
        })
    })
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
        if let Some(name) = names.iter().find(|name| name.len() > MAX_TOOL_STRING_BYTES) {
            return Err(HarnessError::ComfyUi(format!(
                "A checkpoint name of {} bytes is too long to return whole; a checkpoint name is never shortened.",
                name.len()
            )));
        }
        let harness = Arc::clone(&self.0);
        let discovery = tokio::task::spawn_blocking(move || harness.discover_packs())
            .await
            .map_err(|error| HarnessError::Configuration(error.to_string()))?;
        let problems: Vec<String> = discovery
            .all_diagnostics()
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.path.display(), diagnostic.message))
            .collect();
        let after: Vec<&String> = names
            .iter()
            .filter(|name| {
                arguments
                    .cursor
                    .as_ref()
                    .map_or(true, |cursor| *name > cursor)
            })
            .collect();
        let mut result = budgeted_page("checkpoints", limit, |count| {
            let page = &after[..count.min(after.len())];
            let rows = page
                .iter()
                .map(|name| {
                    let profiles = discovery
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
            let next = (after.len() > page.len())
                .then(|| page.last().map(|name| (*name).clone()))
                .flatten();
            std::future::ready(Ok((rows, next)))
        })
        .await?;
        result["profile_note"] = json!(PROFILE_NOTE);
        if let Some(problems) = pack_problems(&problems) {
            result["profile_pack_problems"] = problems;
        }
        Ok(result)
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
        match (
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
            _ => {}
        }
        budgeted_page("runs", limit, |count| self.page(&arguments, count)).await
    }
}

impl ListRunsTool {
    async fn page(
        &self,
        arguments: &RunsArguments,
        count: usize,
    ) -> Result<(Vec<Value>, Option<String>)> {
        let page = match &arguments.character_id {
            Some(character_id) => {
                self.0
                    .characters()
                    .list_runs(
                        character_id,
                        CharacterRunQuery {
                            spec_version: arguments.spec_version,
                            status: arguments.status,
                            limit: Some(count),
                            cursor: arguments.cursor.clone(),
                        },
                    )
                    .await?
            }
            None => {
                self.0
                    .repository()
                    .as_async()
                    .list_page(RunListQuery {
                        status: arguments.status,
                        workflow: arguments.workflow.clone(),
                        limit: Some(count),
                        cursor: arguments.cursor.clone(),
                        ..Default::default()
                    })
                    .await?
            }
        };
        let mut rows = Vec::with_capacity(page.runs.len());
        for run in &page.runs {
            rows.push(self.row(run).await?);
        }
        Ok((rows, page.next_cursor))
    }

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
            "error": run.error.as_deref().map(|error| cap_text(error, ERROR_CAP_BYTES))
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
        match (
            &arguments.run_id,
            &arguments.character_id,
            arguments.spec_version,
        ) {
            (Some(_), Some(_), _) => {
                return Err(invalid("run_id cannot be combined with character_id"))
            }
            (_, None, Some(_)) => return Err(invalid("spec_version requires character_id")),
            _ => {}
        }
        let arguments = &arguments;
        budgeted_page("outputs", limit, |count| async move {
            let page = match &arguments.character_id {
                Some(character_id) => {
                    self.0
                        .characters()
                        .list_outputs(
                            character_id,
                            CharacterRunQuery {
                                spec_version: arguments.spec_version,
                                status: None,
                                limit: Some(count),
                                cursor: arguments.cursor.clone(),
                            },
                        )
                        .await?
                }
                None => {
                    self.0
                        .repository()
                        .as_async()
                        .list_outputs(OutputListQuery {
                            run: arguments.run_id.clone(),
                            limit: Some(count),
                            cursor: arguments.cursor.clone(),
                            ..Default::default()
                        })
                        .await?
                }
            };
            Ok((
                page.outputs.into_iter().map(output_metadata).collect(),
                page.next_cursor,
            ))
        })
        .await
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
        let arguments = &arguments;
        budgeted_page("characters", limit, |count| async move {
            let page = self
                .0
                .characters()
                .list_characters(CharacterListQuery {
                    limit: Some(count),
                    cursor: arguments.cursor.clone(),
                })
                .await?;
            let characters = page
                .characters
                .iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok((characters, page.next_cursor))
        })
        .await
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
        "Return one character with a summary of its latest spec version (version, spec_bytes, element_count; the spec content is NOT included, read it with get_character_spec), its active reference images (informational only; the user chooses image inputs) and a summary of current decisions: counts and the most recent approved outputs."
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
        let latest_spec = service
            .latest_spec_version(&character_id)
            .await?
            .map(|latest| spec_summary(&latest))
            .transpose()?;
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
            "latest_spec": latest_spec,
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

fn spec_summary(version: &CharacterSpecVersion) -> Result<Value> {
    Ok(json!({
        "version": version.version,
        "created_at": version.created_at,
        "based_on": version.based_on,
        "note": version.note,
        "spec_bytes": serde_json::to_vec(&version.spec)?.len(),
        "element_count": version.spec.elements.len(),
        "palette_count": version.spec.palette.len(),
        "content_included": false,
        "read_with": "Call get_character_spec with this character_id (and this version) and follow next_cursor until complete is true; only then have you read the whole spec."
    }))
}

struct GetCharacterSpecTool(Arc<Harness>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecArguments {
    character_id: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

fn spec_cursor(version: u32, offset: usize) -> String {
    format!("spec.v{version}.e{offset}")
}

fn parse_spec_cursor(cursor: &str) -> Result<(u32, usize)> {
    cursor
        .strip_prefix("spec.v")
        .and_then(|rest| rest.split_once(".e"))
        .and_then(|(version, offset)| Some((version.parse().ok()?, offset.parse().ok()?)))
        .ok_or_else(|| {
            invalid(
                "cursor is not a get_character_spec cursor; pass next_cursor exactly as returned",
            )
        })
}

#[async_trait]
impl AssistantTool for GetCharacterSpecTool {
    fn name(&self) -> &str {
        "get_character_spec"
    }

    fn description(&self) -> &str {
        "Read one character spec version in pages of whole elements, in spec order (version defaults to the latest). The first page carries summary, style_notes, palette and views; every page carries version, elements_total, elements, next_cursor and complete. Nothing is shortened. The spec is fully read only when complete is true; follow next_cursor (with the same character_id) until then, and never build a proposal from an incomplete read. State the spec version you used."
    }

    fn json_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "character_id": {"type": "string", "minLength": 1},
                "version": {"type": "integer", "minimum": 1},
                "cursor": {"type": "string", "minLength": 1}
            },
            "required": ["character_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn invoke(&self, _context: ToolContext, arguments: Value) -> Result<Value> {
        let SpecArguments {
            character_id,
            version,
            cursor,
        } = serde_json::from_value(arguments)?;
        let (cursor_version, offset) = match cursor.as_deref().map(parse_spec_cursor).transpose()? {
            Some((version, offset)) => (Some(version), offset),
            None => (None, 0),
        };
        if matches!((version, cursor_version), (Some(a), Some(b)) if a != b) {
            return Err(invalid(
                "version does not match the cursor; omit version or pass the cursor's version",
            ));
        }
        let service = self.0.characters();
        service.get_character(&character_id).await?;
        let record = match version.or(cursor_version) {
            Some(version) => service.get_spec_version(&character_id, version).await?,
            None => service
                .latest_spec_version(&character_id)
                .await?
                .ok_or_else(|| invalid("this character has no spec version yet"))?,
        };
        spec_page(&record, offset, cursor.is_some())
    }
}

/// One page of a spec: whole elements under the page budget, never run
/// through the generic trimmer. A field that cannot fit is an error.
fn spec_page(record: &CharacterSpecVersion, offset: usize, continued: bool) -> Result<Value> {
    let CharacterSpec {
        summary,
        elements,
        palette,
        views,
        style_notes,
    } = &record.spec;
    if offset > elements.len() || (continued && offset == 0) {
        return Err(invalid("cursor is outside this spec version"));
    }
    let mut page = json!({
        "character_id": record.character_id,
        "version": record.version,
        "elements_total": elements.len(),
        "elements_from": offset
    });
    if !continued {
        for (name, value) in [
            ("summary", json!(summary)),
            ("style_notes", json!(style_notes)),
            ("palette", json!(palette)),
            ("views", json!(views)),
        ] {
            if json_size(&value) > PAGE_BUDGET_BYTES {
                return Err(invalid(&format!(
                    "The spec field {name} is {} bytes and cannot be returned whole within the {PAGE_BUDGET_BYTES}-byte page budget.",
                    json_size(&value)
                )));
            }
            page[name] = value;
        }
    }
    let mut used = json_size(&page);
    let mut included = Vec::new();
    for element in &elements[offset..] {
        let element = serde_json::to_value(element)?;
        let size = json_size(&element) + 1;
        if used + size > PAGE_BUDGET_BYTES {
            break;
        }
        used += size;
        included.push(element);
    }
    let end = offset + included.len();
    if included.is_empty() && end < elements.len() {
        return Err(invalid(
            "The next spec element cannot be returned whole within the page budget.",
        ));
    }
    let next_cursor = (end < elements.len()).then(|| spec_cursor(record.version, end));
    page["elements"] = Value::Array(included);
    page["complete"] = json!(next_cursor.is_none());
    page["next_cursor"] = json!(next_cursor);
    page["note"] = json!(if next_cursor.is_some() {
        "Incomplete: more elements follow. Pass next_cursor as cursor to read the rest before building any proposal from this spec."
    } else {
        "Complete: this page is the last. State the version number when you use this spec."
    });
    if json_size(&page) > MAX_TOOL_RESULT_BYTES {
        return Err(invalid("The spec page exceeds the tool result cap."));
    }
    Ok(page)
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
        "List recorded annotations for an output, newest first (at most 10; truncated says when more exist). Each carries a label; they are model opinions, not facts or user approval. Long text is shortened with a marker; prompts and raw model responses are omitted."
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
        let fetched = annotations.len();
        let mut used = 0;
        let rows: Vec<Value> = annotations
            .into_iter()
            .take(MAX_ANNOTATIONS)
            .map(annotation_row)
            .take_while(|row| {
                used += json_size(row) + 1;
                used <= PAGE_BUDGET_BYTES
            })
            .collect();
        let truncated = fetched > rows.len() || fetched >= OUTPUT_ANNOTATION_LIMIT;
        Ok(bounded_tool_result(json!({
            "output_id": output_id,
            "source": "model_opinion",
            "note": "These are model opinions, not provenance and not user approval.",
            "annotations": rows,
            "limit": MAX_ANNOTATIONS,
            "truncated": truncated,
            "truncation_note": truncated.then_some("Older or further annotations exist and are not shown; this is not the complete list.")
        })))
    }
}

fn annotation_label(kind: &str) -> String {
    match kind {
        "vision_review" => "model opinion".to_owned(),
        other => format!("model opinion (kind {})", cap_text(other, 64)),
    }
}

fn annotation_row(annotation: crate::OutputAnnotation) -> Value {
    json!({
        "id": annotation.id,
        "kind": annotation.kind,
        "label": annotation_label(&annotation.kind),
        "model": annotation.model,
        "created_at": annotation.created_at,
        "caption": cap_text(&annotation.caption, ANNOTATION_TEXT_CAP_BYTES),
        "adherence_critique": cap_text(&annotation.adherence_critique, ANNOTATION_TEXT_CAP_BYTES),
        "tags": annotation.tags
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_labels_never_name_a_role_and_unknown_kinds_pass_through() {
        assert_eq!(annotation_label("vision_review"), "model opinion");
        assert_eq!(
            annotation_label("future_kind"),
            "model opinion (kind future_kind)"
        );
    }

    #[test]
    fn capped_text_is_marked_and_respects_character_boundaries() {
        assert_eq!(cap_text("short", 10), "short");
        assert_eq!(cap_text("ééé", 3), "é…(truncated)");
    }

    #[test]
    fn a_maximal_spec_pages_whole_elements_without_trimming() {
        use crate::characters::{
            ElementCategory, Requirement, SpecElement, MAX_ELEMENTS, STYLE_NOTES_MAX_BYTES,
        };
        let record = CharacterSpecVersion {
            character_id: "c".into(),
            version: 4,
            created_at: chrono::Utc::now(),
            based_on: None,
            note: None,
            spec: CharacterSpec {
                summary: "s".repeat(2_000),
                style_notes: Some("n".repeat(STYLE_NOTES_MAX_BYTES)),
                palette: Vec::new(),
                views: Vec::new(),
                elements: (0..MAX_ELEMENTS)
                    .map(|index| SpecElement {
                        id: format!("e{index}"),
                        category: ElementCategory::Other,
                        description: "d".repeat(1_000),
                        requirement: Requirement::Flexible,
                    })
                    .collect(),
            },
        };
        let (mut offset, mut seen, mut pages) = (0, 0, 0);
        loop {
            let page = spec_page(&record, offset, offset > 0).unwrap();
            assert!(page.get("truncation_notes").is_none() && page.get("truncated").is_none());
            assert!(json_size(&page) <= MAX_TOOL_RESULT_BYTES);
            assert_eq!(page["elements_from"], offset);
            seen += page["elements"].as_array().unwrap().len();
            pages += 1;
            match page["next_cursor"].as_str() {
                Some(cursor) => {
                    assert_eq!(page["complete"], false);
                    offset = parse_spec_cursor(cursor).unwrap().1;
                }
                None => {
                    assert_eq!(page["complete"], true);
                    break;
                }
            }
        }
        assert_eq!((seen, pages > 1), (MAX_ELEMENTS, true));
        let mut oversized = record.clone();
        oversized.spec.summary = "x".repeat(PAGE_BUDGET_BYTES);
        let error = spec_page(&oversized, 0, false).unwrap_err().to_string();
        assert!(error.contains("summary"), "{error}");
    }

    #[test]
    fn spec_cursors_round_trip_and_reject_foreign_text() {
        assert_eq!(parse_spec_cursor(&spec_cursor(3, 17)).unwrap(), (3, 17));
        assert!(parse_spec_cursor("v1.abcd").is_err());
    }
}
