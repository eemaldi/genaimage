use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::{
    orchestrator::WorkflowDiscovery, ChatDelta, ChatMessage, ChatRequest, Harness, HarnessError,
    LlmRoleBinding, LoadedWorkflow,
};

use super::{PromptTemplate, PromptTemplates};

pub const SUGGEST_PROMPT_TEMPLATE: &str = "suggest";
pub const DEFAULT_SUGGEST_PROMPT: &str = r#"You are the GenAImage workflow suggestion assistant. Use only the workflow catalogue provided by the user request. Propose, but never submit, generation runs. Do not invent or correct workflow IDs, preset names, parameter names, or values. Do not treat a technical proposal as creative approval.

Return exactly one JSON object with this shape and no Markdown fence or extra text:
{"suggestions":[{"rank":1,"workflow_id":"id","preset":"preset-name-or-null","parameters":{"name":"exact-json-value"},"rationale":"short reason","parameter_notes":{}}]}

Order suggestions by your ranking. Use a JSON string for preset or null when no preset should be selected. `parameters` contains proposed overrides only. Return at most the requested limit, omit duplicates, and use exact JSON number representations. The user message supplies the generation intent and the complete bounded catalogue summary.
"#;

pub const SUGGESTION_CATALOGUE_MAX_BYTES: usize = 64 * 1024;
const SUGGESTION_OUTPUT_MAX_BYTES: usize = 256 * 1024;
const MAX_FIELD_BYTES: usize = 2 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuggestionRequest {
    pub intent: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProposalIssue {
    pub field: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProposal {
    pub workflow_revision: String,
    pub preset: Option<String>,
    pub parameters: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowSuggestion {
    pub workflow_id: String,
    pub workflow_revision: String,
    pub rank: usize,
    pub preset: Option<String>,
    pub parameters: BTreeMap<String, Value>,
    pub rationale: String,
    pub parameter_notes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DroppedSuggestion {
    pub workflow_id: String,
    pub field: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct SuggestionResult {
    pub model: String,
    pub template: PromptTemplate,
    pub suggestions: Vec<WorkflowSuggestion>,
    pub dropped: Vec<DroppedSuggestion>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
struct CatalogueSnapshot {
    json: String,
    revisions: BTreeMap<String, String>,
    truncated: bool,
}

#[derive(Debug, Error)]
pub enum SuggestionError {
    #[error("invalid assistant suggestion request: {0}")]
    InvalidRequest(#[source] HarnessError),
    #[error("assistant provider request failed: {0}")]
    Provider(#[source] HarnessError),
    #[error("assistant provider returned malformed suggestion JSON: {0}")]
    MalformedResponse(String),
    #[error("workflow suggestion failed: {0}")]
    Internal(#[source] HarnessError),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmSuggestionResponse {
    suggestions: Vec<LlmWorkflowSuggestion>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmWorkflowSuggestion {
    rank: u64,
    workflow_id: String,
    preset: Option<String>,
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
    rationale: String,
    #[serde(default)]
    parameter_notes: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageUploadPolicy {
    Reject,
    Allow,
    OmitMissing,
}

pub fn validate_proposal(
    workflow: &LoadedWorkflow,
    preset: Option<&str>,
    parameters: &BTreeMap<String, Value>,
) -> std::result::Result<ValidatedProposal, Vec<ProposalIssue>> {
    validate_proposal_with_options(workflow, preset, parameters, ImageUploadPolicy::Reject)
}

pub fn validate_proposal_with_options(
    workflow: &LoadedWorkflow,
    preset: Option<&str>,
    parameters: &BTreeMap<String, Value>,
    image_uploads: ImageUploadPolicy,
) -> std::result::Result<ValidatedProposal, Vec<ProposalIssue>> {
    let preset_error = workflow.preset_values(preset).err();
    let mut issues: Vec<ProposalIssue> = preset_error
        .iter()
        .map(|error| ProposalIssue {
            field: "preset".into(),
            reason: error.to_string(),
        })
        .collect();
    if image_uploads != ImageUploadPolicy::Allow
        && preset
            .and_then(|name| workflow.manifest.presets.get(name))
            .is_some_and(|values| {
                values.keys().any(|name| {
                    workflow
                        .manifest
                        .parameters
                        .get(name)
                        .is_some_and(|definition| {
                            definition.kind == crate::ParameterKind::ImageUpload
                        })
                })
            })
    {
        issues.push(ProposalIssue {
            field: "preset".into(),
            reason: "preset binds an image input the user must choose".into(),
        });
    }
    if image_uploads != ImageUploadPolicy::Allow {
        issues.extend(
            parameters
                .keys()
                .filter(|name| {
                    workflow
                        .manifest
                        .parameters
                        .get(*name)
                        .is_some_and(|parameter| {
                            parameter.kind == crate::ParameterKind::ImageUpload
                        })
                })
                .map(|name| ProposalIssue {
                    field: format!("parameters.{name}"),
                    reason: "image inputs must be chosen by the user".into(),
                }),
        );
    }
    if preset_error.is_some() {
        for (name, value) in parameters {
            let issue = match workflow.manifest.parameters.get(name) {
                Some(definition) => definition.validate_value(name, value).err(),
                None => Some(HarnessError::Workflow(format!(
                    "parameter '{name}' is not defined by workflow '{}'",
                    workflow.manifest.id
                ))),
            };
            if let Some(error) = issue {
                issues.push(ProposalIssue {
                    field: format!("parameters.{name}"),
                    reason: error.to_string(),
                });
            }
        }
    } else {
        match workflow.validate_bindings(preset, parameters) {
            Ok(errors) => issues.extend(errors.into_iter().filter_map(|error| {
                if image_uploads == ImageUploadPolicy::OmitMissing
                    && !parameters.contains_key(&error.parameter)
                    && workflow
                        .manifest
                        .parameters
                        .get(&error.parameter)
                        .is_some_and(|parameter| {
                            parameter.kind == crate::ParameterKind::ImageUpload
                        })
                {
                    return None;
                }
                Some(ProposalIssue {
                    field: format!("parameters.{}", error.parameter),
                    reason: error.reason,
                })
            })),
            Err(error) => issues.push(ProposalIssue {
                field: "preset".into(),
                reason: error.to_string(),
            }),
        }
    }
    issues.sort_by(|left, right| left.field.cmp(&right.field));
    issues.dedup_by(|left, right| left.field == right.field && left.reason == right.reason);
    if issues.is_empty() {
        Ok(ValidatedProposal {
            workflow_revision: workflow.revision(),
            preset: preset.map(str::to_owned),
            parameters: parameters.clone(),
        })
    } else {
        Err(issues)
    }
}

pub async fn suggest_workflows(
    harness: Arc<Harness>,
    discovery: WorkflowDiscovery,
    binding: LlmRoleBinding,
    request: SuggestionRequest,
) -> std::result::Result<SuggestionResult, SuggestionError> {
    let mut templates = PromptTemplates::new(harness.config().data_dir());
    templates
        .register_builtin(SUGGEST_PROMPT_TEMPLATE, DEFAULT_SUGGEST_PROMPT)
        .map_err(SuggestionError::Internal)?;
    let template = templates
        .load(SUGGEST_PROMPT_TEMPLATE)
        .await
        .map_err(SuggestionError::Internal)?;
    let catalogue = catalogue(&discovery);
    let context = format!(
        "Intent: {}\nRequested maximum: {}\nCatalogue truncated: {}\nWorkflow catalogue JSON:\n{}",
        request.intent, request.limit, catalogue.truncated, catalogue.json
    );
    let prompt = if template.content.contains("{{context}}") {
        template.content.replace("{{context}}", &context)
    } else {
        format!("{}\n\n{context}", template.content)
    };
    let chat_request = ChatRequest {
        model: binding.model().to_owned(),
        messages: vec![ChatMessage::User(vec![crate::ChatContent::Text(prompt)])],
        tools: Vec::new(),
    };
    binding
        .check_request(&chat_request)
        .map_err(SuggestionError::InvalidRequest)?;
    let mut stream = binding
        .provider()
        .chat_stream(chat_request)
        .await
        .map_err(SuggestionError::Provider)?;
    let mut response = String::new();
    while let Some(delta) = stream.next().await {
        match delta.map_err(SuggestionError::Provider)? {
            ChatDelta::Text(text) => {
                response.push_str(&text);
                if response.len() > SUGGESTION_OUTPUT_MAX_BYTES {
                    return Err(SuggestionError::MalformedResponse(format!(
                        "response exceeded the {SUGGESTION_OUTPUT_MAX_BYTES}-byte limit"
                    )));
                }
            }
            ChatDelta::ToolCall(_) => {
                return Err(SuggestionError::MalformedResponse(
                    "tool calls are not accepted in a suggestion response".into(),
                ));
            }
        }
    }
    let parsed: LlmSuggestionResponse = serde_json::from_str(&response)
        .map_err(|error| SuggestionError::MalformedResponse(error.to_string()))?;
    let model = binding.model().to_owned();
    let (suggestions, dropped) = validate_ranked(
        harness,
        parsed.suggestions,
        request.limit,
        catalogue.revisions,
    )
    .await?;
    Ok(SuggestionResult {
        model,
        template,
        suggestions,
        dropped,
        truncated: catalogue.truncated,
    })
}

async fn validate_ranked(
    harness: Arc<Harness>,
    mut suggestions: Vec<LlmWorkflowSuggestion>,
    limit: usize,
    revisions: BTreeMap<String, String>,
) -> std::result::Result<(Vec<WorkflowSuggestion>, Vec<DroppedSuggestion>), SuggestionError> {
    suggestions.sort_by_key(|suggestion| suggestion.rank);
    tokio::task::spawn_blocking(move || {
        let mut valid = Vec::new();
        let mut dropped = Vec::new();
        let mut seen = BTreeSet::new();
        for suggestion in suggestions {
            let workflow_id = suggestion.workflow_id.clone();
            let expected_revision = match revisions.get(&workflow_id) {
                Some(revision) => revision,
                None => {
                    dropped.push(DroppedSuggestion {
                        workflow_id,
                        field: "workflow_id".into(),
                        reason: "workflow was not in the catalogue snapshot sent to the LLM".into(),
                    });
                    continue;
                }
            };
            let workflow = match harness.load_workflow_by_id(&workflow_id) {
                Ok(Some(workflow)) => workflow,
                Ok(None) => {
                    dropped.push(DroppedSuggestion {
                        workflow_id: workflow_id.clone(),
                        field: "workflow_id".into(),
                        reason: "workflow changed during suggestion".into(),
                    });
                    continue;
                }
                Err(error) => {
                    dropped.push(DroppedSuggestion {
                        workflow_id: workflow_id.clone(),
                        field: "workflow_id".into(),
                        reason: format!("workflow changed during suggestion: {error}"),
                    });
                    continue;
                }
            };
            if workflow.revision() != *expected_revision {
                dropped.push(DroppedSuggestion {
                    workflow_id,
                    field: "workflow_id".into(),
                    reason: "workflow changed during suggestion".into(),
                });
                continue;
            }
            if suggestion.preset.as_deref().is_some_and(|preset| {
                workflow.manifest.presets.get(preset).is_some_and(|values| {
                    values.keys().any(|name| {
                        workflow
                            .manifest
                            .parameters
                            .get(name)
                            .is_some_and(|definition| {
                                definition.kind == crate::ParameterKind::ImageUpload
                            })
                    })
                })
            }) {
                dropped.push(DroppedSuggestion {
                    workflow_id,
                    field: "preset".into(),
                    reason: "preset binds an image input the user must choose".into(),
                });
                continue;
            }
            let mut parameters = suggestion.parameters;
            for (name, definition) in &workflow.manifest.parameters {
                if definition.kind == crate::ParameterKind::ImageUpload
                    && parameters.remove(name).is_some()
                {
                    dropped.push(DroppedSuggestion {
                        workflow_id: workflow_id.clone(),
                        field: format!("parameters.{name}"),
                        reason: "image inputs must be chosen by the user".into(),
                    });
                }
            }
            let proposal = match validate_proposal_with_options(
                &workflow,
                suggestion.preset.as_deref(),
                &parameters,
                ImageUploadPolicy::OmitMissing,
            ) {
                Ok(proposal) => proposal,
                Err(issues) => {
                    dropped.extend(issues.into_iter().map(|issue| DroppedSuggestion {
                        workflow_id: workflow_id.clone(),
                        field: issue.field,
                        reason: issue.reason,
                    }));
                    continue;
                }
            };
            if !seen.insert(workflow_id.clone()) {
                dropped.push(DroppedSuggestion {
                    workflow_id,
                    field: "workflow_id".into(),
                    reason: "duplicate workflow suggestion was removed".into(),
                });
                continue;
            }
            if valid.len() < limit {
                valid.push(WorkflowSuggestion {
                    workflow_id,
                    workflow_revision: proposal.workflow_revision,
                    rank: valid.len() + 1,
                    preset: proposal.preset,
                    parameters: proposal.parameters,
                    rationale: suggestion.rationale,
                    parameter_notes: suggestion.parameter_notes,
                });
            }
        }
        Ok((valid, dropped))
    })
    .await
    .map_err(|error| {
        SuggestionError::Internal(HarnessError::Workflow(format!(
            "workflow suggestion validation task failed: {error}"
        )))
    })?
}

fn catalogue(discovery: &WorkflowDiscovery) -> CatalogueSnapshot {
    let indexable: BTreeSet<_> = discovery.indexable_manifest_paths.keys().collect();
    let mut workflows = discovery
        .workflows
        .iter()
        .filter(|workflow| indexable.contains(&workflow.manifest.id))
        .collect::<Vec<_>>();
    workflows.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    let mut entries = Vec::new();
    let mut revisions = BTreeMap::new();
    let mut bytes = 2;
    let mut truncated = false;
    for workflow in workflows {
        truncated |= catalogue_entry_was_truncated(&workflow.manifest);
        let entry = catalogue_entry(&workflow.manifest);
        let encoded = serde_json::to_vec(&entry).expect("catalogue values are serialisable");
        let separator = usize::from(!entries.is_empty());
        if bytes + separator + encoded.len() > SUGGESTION_CATALOGUE_MAX_BYTES {
            truncated = true;
            continue;
        }
        bytes += separator + encoded.len();
        revisions.insert(workflow.manifest.id.clone(), workflow.revision.clone());
        entries.push(entry);
    }
    CatalogueSnapshot {
        json: serde_json::to_string(&entries).expect("catalogue values are serialisable"),
        revisions,
        truncated,
    }
}

fn catalogue_entry_was_truncated(manifest: &crate::WorkflowManifest) -> bool {
    let string_too_long = |value: &str| value.len() > MAX_FIELD_BYTES;
    let value_too_long = |value: &Value| {
        serde_json::to_vec(value)
            .expect("JSON value is serialisable")
            .len()
            > MAX_FIELD_BYTES
    };
    let values_too_long = |values: &Option<Vec<Value>>| {
        values
            .as_ref()
            .is_some_and(|values| value_too_long(&json!(values)))
    };
    manifest.title.len() > MAX_FIELD_BYTES
        || manifest.description.as_deref().is_some_and(string_too_long)
        || manifest
            .tags
            .as_deref()
            .is_some_and(|tags| tags.iter().any(|tag| string_too_long(tag)))
        || value_too_long(&json!(manifest.presets))
        || manifest.parameters.values().any(|parameter| {
            string_too_long(&parameter.description)
                || parameter.default.as_ref().is_some_and(value_too_long)
                || values_too_long(&parameter.allowed_values)
                || parameter.widget.as_deref().is_some_and(string_too_long)
                || values_too_long(&parameter.options)
                || parameter.group.as_deref().is_some_and(string_too_long)
                || parameter
                    .placeholder
                    .as_deref()
                    .is_some_and(string_too_long)
        })
}

fn catalogue_entry(manifest: &crate::WorkflowManifest) -> Value {
    let parameters = manifest
        .parameters
        .iter()
        .filter(|(_, parameter)| parameter.kind != crate::ParameterKind::ImageUpload)
        .map(|(name, parameter)| {
            let mut summary = Map::new();
            let kind = match parameter.kind {
                crate::ParameterKind::String => "string",
                crate::ParameterKind::Integer => "integer",
                crate::ParameterKind::Number => "number",
                crate::ParameterKind::Boolean => "boolean",
                crate::ParameterKind::Seed => "seed",
                crate::ParameterKind::ImageUpload => "image_upload",
            };
            summary.insert("type".into(), json!(kind));
            summary.insert("required".into(), json!(parameter.required));
            summary.insert(
                "description".into(),
                json!(bounded_string(&parameter.description)),
            );
            summary.insert("default".into(), bounded_value(parameter.default.as_ref()));
            summary.insert(
                "allowed_values".into(),
                bounded_values(parameter.allowed_values.as_ref()),
            );
            summary.insert("minimum".into(), json!(parameter.minimum));
            summary.insert("maximum".into(), json!(parameter.maximum));
            summary.insert(
                "widget".into(),
                bounded_optional_string(parameter.widget.as_ref()),
            );
            summary.insert("step".into(), json!(parameter.step));
            summary.insert("options".into(), bounded_values(parameter.options.as_ref()));
            summary.insert(
                "group".into(),
                bounded_optional_string(parameter.group.as_ref()),
            );
            summary.insert("advanced".into(), json!(parameter.advanced));
            summary.insert(
                "placeholder".into(),
                bounded_optional_string(parameter.placeholder.as_ref()),
            );
            (name.clone(), Value::Object(summary))
        })
        .collect::<serde_json::Map<String, Value>>();
    json!({
        "id": manifest.id,
        "name": bounded_string(&manifest.title),
        "description": manifest.description.as_deref().map(bounded_string),
        "tags": manifest.tags.as_ref().map(|tags| tags.iter().map(|tag| bounded_string(tag)).collect::<Vec<_>>()),
        "presets": bounded_presets(manifest),
        "parameters": parameters,
    })
}

/// Image-binding presets are omitted from suggestions rather than exposing
/// upload IDs or silently carrying an image value. Selecting one is rejected.
fn bounded_presets(manifest: &crate::WorkflowManifest) -> Value {
    let presets = manifest
        .presets
        .iter()
        .filter(|(_, values)| {
            !values.keys().any(|name| {
                manifest
                    .parameters
                    .get(name)
                    .is_some_and(|definition| definition.kind == crate::ParameterKind::ImageUpload)
            })
        })
        .map(|(name, values)| (name.clone(), values.clone()))
        .collect::<BTreeMap<_, _>>();
    bounded_serialised_value(&json!(presets))
}

fn bounded_values(values: Option<&Vec<Value>>) -> Value {
    values
        .map(|values| bounded_serialised_value(&json!(values)))
        .unwrap_or(Value::Null)
}

fn bounded_value(value: Option<&Value>) -> Value {
    value.map(bounded_serialised_value).unwrap_or(Value::Null)
}

fn bounded_serialised_value(value: &Value) -> Value {
    let encoded = serde_json::to_vec(value).expect("JSON value is serialisable");
    if encoded.len() <= MAX_FIELD_BYTES {
        value.clone()
    } else {
        json!({"omitted": format!("value exceeds {MAX_FIELD_BYTES} bytes")})
    }
}

fn bounded_optional_string(value: Option<&String>) -> Value {
    value
        .map(|value| Value::String(bounded_string(value)))
        .unwrap_or(Value::Null)
}

fn bounded_string(value: &str) -> String {
    if value.len() <= MAX_FIELD_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FIELD_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
