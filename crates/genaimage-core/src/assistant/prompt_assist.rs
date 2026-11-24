use std::collections::HashSet;

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;

use crate::{
    ChatContent, ChatDelta, ChatMessage, ChatRequest, HarnessError, LlmRoleBinding, LoadedWorkflow,
    Result,
};

use super::PromptTemplates;

pub const PROMPT_MAX_CHARS: usize = 16 * 1024;
pub const INSTRUCTION_MAX_CHARS: usize = 8 * 1024;
pub const ORIGINAL_MAX_CHARS: usize = 16 * 1024;
pub const LLM_RESPONSE_MAX_BYTES: usize = 64 * 1024;

pub const EXPAND_TEMPLATE: &str = "prompt_expand";
pub const REFINE_TEMPLATE: &str = "prompt_refine";
pub const NEGATIVE_TEMPLATE: &str = "prompt_negative";
pub const VARIATIONS_TEMPLATE: &str = "prompt_variations";

const DEFAULT_EXPAND_TEMPLATE: &str = r#"Expand the user's prompt into one clearer, more detailed image-generation prompt. Preserve every explicit subject, model, workflow, seed, image and setting. Treat the result as an editable proposal; do not claim it is applied. Return only one JSON object shaped exactly as {"proposals":["..."]}, with no markdown or prose.
"#;
const DEFAULT_REFINE_TEMPLATE: &str = r#"Refine the user's prompt according to the supplied instruction. Preserve explicit requirements that the instruction does not change. Treat the result as an editable proposal; do not claim it is applied. Return only one JSON object shaped exactly as {"proposals":["..."]}, with no markdown or prose.
"#;
const DEFAULT_NEGATIVE_TEMPLATE: &str = r#"Create an effective negative prompt for the supplied positive prompt. If an existing negative prompt is present, revise it; otherwise propose a new one. Preserve explicit constraints. Treat the result as an editable proposal; do not claim it is applied. Return only one JSON object shaped exactly as {"proposals":["..."]}, with no markdown or prose.
"#;
const DEFAULT_VARIATIONS_TEMPLATE: &str = r#"Create the requested number of distinct, useful prompt variations. Preserve every explicit subject, model, workflow, seed, image and setting unless a variation deliberately explores composition or style without silently changing constrained choices. Treat all results as editable proposals; do not claim they are applied. Return only one JSON object shaped exactly as {"proposals":["..."]}, with no markdown or prose. Do not invent extra entries to reach the requested count.
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptAssistMode {
    Expand,
    Refine,
    Negative,
    Variations,
}

impl PromptAssistMode {
    pub const fn template_name(self) -> &'static str {
        match self {
            Self::Expand => EXPAND_TEMPLATE,
            Self::Refine => REFINE_TEMPLATE,
            Self::Negative => NEGATIVE_TEMPLATE,
            Self::Variations => VARIATIONS_TEMPLATE,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PromptAssistRequest {
    pub mode: PromptAssistMode,
    pub prompt: String,
    pub instruction: Option<String>,
    pub n: Option<usize>,
    pub original: Option<String>,
    pub workflow_id: Option<String>,
    pub parameter: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptDiff {
    pub op: DiffOperation,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffOperation {
    Equal,
    Insert,
    Delete,
}

#[derive(Clone, Debug)]
pub struct PromptProposal {
    pub text: String,
    pub diff: Vec<PromptDiff>,
}

#[derive(Clone, Debug)]
pub struct PromptTemplateReference {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct PromptAssistResponse {
    pub proposals: Vec<PromptProposal>,
    pub proposal_count: usize,
    pub model: String,
    pub mode: PromptAssistMode,
    pub template: PromptTemplateReference,
}

#[derive(Clone)]
pub struct PromptAssistService {
    templates: PromptTemplates,
}

impl PromptAssistService {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Result<Self> {
        let mut templates = PromptTemplates::new(data_dir);
        templates.register_builtin(EXPAND_TEMPLATE, DEFAULT_EXPAND_TEMPLATE)?;
        templates.register_builtin(REFINE_TEMPLATE, DEFAULT_REFINE_TEMPLATE)?;
        templates.register_builtin(NEGATIVE_TEMPLATE, DEFAULT_NEGATIVE_TEMPLATE)?;
        templates.register_builtin(VARIATIONS_TEMPLATE, DEFAULT_VARIATIONS_TEMPLATE)?;
        Ok(Self { templates })
    }

    pub fn validate(
        &self,
        request: &PromptAssistRequest,
        workflow: Option<&LoadedWorkflow>,
    ) -> Result<()> {
        validate_request(request, workflow)
    }

    pub async fn assist(
        &self,
        binding: &LlmRoleBinding,
        request: PromptAssistRequest,
        workflow: Option<&LoadedWorkflow>,
    ) -> Result<PromptAssistResponse> {
        validate_request(&request, workflow)?;
        let template = self.templates.load(request.mode.template_name()).await?;
        let user = request_message(&request, workflow)?;
        let chat = ChatRequest {
            model: binding.model().to_owned(),
            messages: vec![
                ChatMessage::System(template.content),
                ChatMessage::User(vec![ChatContent::Text(user)]),
            ],
            tools: Vec::new(),
        };
        binding.check_request(&chat)?;
        let mut stream = binding
            .provider()
            .chat_stream(chat)
            .await
            .map_err(|error| {
                HarnessError::PromptAssistProvider(format!("LLM prompt request failed: {error}"))
            })?;
        let mut response = String::new();
        while let Some(delta) = stream.next().await {
            match delta.map_err(|error| {
                HarnessError::PromptAssistProvider(format!("LLM prompt stream failed: {error}"))
            })? {
                ChatDelta::Text(text) => {
                    if response.len().saturating_add(text.len()) > LLM_RESPONSE_MAX_BYTES {
                        return Err(HarnessError::PromptAssistProvider(format!(
                            "LLM response exceeded {LLM_RESPONSE_MAX_BYTES} bytes"
                        )));
                    }
                    response.push_str(&text);
                }
                ChatDelta::ToolCall(_) => {
                    return Err(HarnessError::PromptAssistProvider(
                        "LLM returned a tool call for a proposal-only prompt request".into(),
                    ));
                }
            }
        }

        let parsed: LlmProposalResponse = serde_json::from_str(&response).map_err(|error| {
            HarnessError::PromptAssistProvider(format!(
                "LLM returned malformed strict JSON: {error}"
            ))
        })?;
        let limit = if request.mode == PromptAssistMode::Variations {
            request.n.unwrap_or(4)
        } else {
            1
        };
        let baseline = match request.mode {
            PromptAssistMode::Negative => request.original.as_deref().unwrap_or(""),
            _ => request.prompt.as_str(),
        };
        let mut seen = HashSet::new();
        let mut proposals = Vec::new();
        for candidate in parsed.proposals {
            let text = candidate.trim();
            if text.is_empty() || !seen.insert(text.to_owned()) {
                continue;
            }
            proposals.push(PromptProposal {
                text: text.to_owned(),
                diff: word_diff(baseline, text),
            });
            if proposals.len() == limit {
                break;
            }
        }
        let proposal_count = proposals.len();
        Ok(PromptAssistResponse {
            proposals,
            proposal_count,
            model: binding.model().to_owned(),
            mode: request.mode,
            template: PromptTemplateReference {
                name: template.name,
                sha256: template.sha256,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmProposalResponse {
    proposals: Vec<String>,
}

fn validate_request(
    request: &PromptAssistRequest,
    workflow: Option<&LoadedWorkflow>,
) -> Result<()> {
    if request.prompt.trim().is_empty() {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "prompt must not be empty".into(),
        ));
    }
    if request.prompt.chars().count() > PROMPT_MAX_CHARS {
        return Err(HarnessError::InvalidPromptAssistRequest(format!(
            "prompt exceeds {PROMPT_MAX_CHARS} characters"
        )));
    }
    if request
        .instruction
        .as_ref()
        .is_some_and(|value| value.chars().count() > INSTRUCTION_MAX_CHARS)
    {
        return Err(HarnessError::InvalidPromptAssistRequest(format!(
            "instruction exceeds {INSTRUCTION_MAX_CHARS} characters"
        )));
    }
    if request
        .original
        .as_ref()
        .is_some_and(|value| value.chars().count() > ORIGINAL_MAX_CHARS)
    {
        return Err(HarnessError::InvalidPromptAssistRequest(format!(
            "original exceeds {ORIGINAL_MAX_CHARS} characters"
        )));
    }
    if request.mode == PromptAssistMode::Refine
        && request
            .instruction
            .as_deref()
            .map_or(true, |value| value.trim().is_empty())
    {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "instruction is required for refine mode".into(),
        ));
    }
    if request.mode == PromptAssistMode::Variations
        && request.n.is_some_and(|count| !(1..=8).contains(&count))
    {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "n must be between 1 and 8 for variations mode".into(),
        ));
    }
    if request.mode != PromptAssistMode::Variations && request.n.is_some() {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "n is only valid for variations mode".into(),
        ));
    }
    if request.mode != PromptAssistMode::Negative && request.original.is_some() {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "original is only valid for negative mode".into(),
        ));
    }
    match (request.workflow_id.as_deref(), workflow) {
        (Some(id), Some(loaded)) if loaded.manifest.id == id => {}
        (Some(_), None) => {
            return Err(HarnessError::InvalidPromptAssistRequest(
                "the requested workflow was not loaded".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(HarnessError::InvalidPromptAssistRequest(
                "a workflow was loaded without a workflow_id".into(),
            ));
        }
        (None, None) => {}
        (Some(_), Some(_)) => {
            return Err(HarnessError::InvalidPromptAssistRequest(
                "the loaded workflow does not match workflow_id".into(),
            ));
        }
    }
    if request.parameter.is_some() && workflow.is_none() {
        return Err(HarnessError::InvalidPromptAssistRequest(
            "parameter requires workflow_id".into(),
        ));
    }
    if let Some(parameter) = request.parameter.as_deref() {
        if workflow.is_some_and(|loaded| !loaded.manifest.parameters.contains_key(parameter)) {
            return Err(HarnessError::InvalidPromptAssistRequest(format!(
                "parameter '{parameter}' was not declared by the selected workflow"
            )));
        }
        if workflow.is_some_and(|loaded| {
            loaded.manifest.parameters[parameter].kind != crate::ParameterKind::String
        }) {
            return Err(HarnessError::InvalidPromptAssistRequest(format!(
                "parameter '{parameter}' is not a string prompt parameter"
            )));
        }
    }
    Ok(())
}

fn request_message(
    request: &PromptAssistRequest,
    workflow: Option<&LoadedWorkflow>,
) -> Result<String> {
    let workflow_context = workflow.map(|loaded| {
        let parameter = request.parameter.as_ref().and_then(|name| {
            loaded.manifest.parameters.get(name).map(|definition| {
                json!({
                    "name": name,
                    "kind": definition.kind,
                    "description": definition.description,
                    "required": definition.required,
                    "default": definition.default,
                    "allowed_values": definition.allowed_values,
                    "minimum": definition.minimum,
                    "maximum": definition.maximum,
                    "widget": definition.widget,
                    "step": definition.step,
                    "options": definition.options,
                    "group": definition.group,
                    "advanced": definition.advanced,
                    "placeholder": definition.placeholder
                })
            })
        });
        json!({
            "id": loaded.manifest.id,
            "description": loaded.manifest.description,
            "tags": loaded.manifest.tags,
            "parameter": parameter
        })
    });
    serde_json::to_string_pretty(&json!({
        "task": {
            "mode": request.mode,
            "prompt": request.prompt,
            "instruction": request.instruction,
            "original": request.original,
            "requested_count": if request.mode == PromptAssistMode::Variations { request.n.unwrap_or(4) } else { 1 },
            "workflow": workflow_context
        },
        "output_contract": {"proposals": ["string"]},
        "safety": "Return only strict JSON matching output_contract. These are proposals only and nothing is applied."
    }))
    .map_err(|error| HarnessError::Configuration(format!("could not encode prompt-assist task: {error}")))
}

pub fn word_diff(original: &str, proposed: &str) -> Vec<PromptDiff> {
    let original = tokens(original);
    let proposed = tokens(proposed);
    if original.len().saturating_mul(proposed.len()) > 1_000_000 {
        let diff = push_diff(Vec::new(), DiffOperation::Delete, original.concat());
        return push_diff(diff, DiffOperation::Insert, proposed.concat());
    }
    let columns = proposed.len() + 1;
    let mut lengths = vec![0_u32; (original.len() + 1) * columns];
    for (row, left) in original.iter().enumerate().rev() {
        for (column, right) in proposed.iter().enumerate().rev() {
            let index = row * columns + column;
            lengths[index] = if left == right {
                lengths[(row + 1) * columns + column + 1] + 1
            } else {
                lengths[(row + 1) * columns + column].max(lengths[row * columns + column + 1])
            };
        }
    }
    let mut result = Vec::new();
    let (mut row, mut column) = (0, 0);
    while row < original.len() && column < proposed.len() {
        if original[row] == proposed[column] {
            result = push_diff(result, DiffOperation::Equal, original[row].clone());
            row += 1;
            column += 1;
        } else if lengths[(row + 1) * columns + column] >= lengths[row * columns + column + 1] {
            result = push_diff(result, DiffOperation::Delete, original[row].clone());
            row += 1;
        } else {
            result = push_diff(result, DiffOperation::Insert, proposed[column].clone());
            column += 1;
        }
    }
    while row < original.len() {
        result = push_diff(result, DiffOperation::Delete, original[row].clone());
        row += 1;
    }
    while column < proposed.len() {
        result = push_diff(result, DiffOperation::Insert, proposed[column].clone());
        column += 1;
    }
    result
}

fn tokens(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut previous_kind = None;
    for character in text.chars() {
        let kind = if character.is_whitespace() {
            0
        } else if character.is_alphanumeric() || character == '_' {
            1
        } else {
            2
        };
        if previous_kind.is_some_and(|value| value != kind) && !current.is_empty() {
            result.push(std::mem::take(&mut current));
        }
        previous_kind = Some(kind);
        current.push(character);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn push_diff(mut diff: Vec<PromptDiff>, op: DiffOperation, text: String) -> Vec<PromptDiff> {
    if text.is_empty() {
        return diff;
    }
    if let Some(last) = diff.last_mut() {
        if last.op == op {
            last.text.push_str(&text);
            return diff;
        }
    }
    diff.push(PromptDiff { op, text });
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_preserves_both_word_sequences() {
        let original = "a bright red cat";
        let proposed = "a vivid red fox";
        let diff = word_diff(original, proposed);
        assert_eq!(
            diff,
            vec![
                PromptDiff {
                    op: DiffOperation::Equal,
                    text: "a ".into()
                },
                PromptDiff {
                    op: DiffOperation::Delete,
                    text: "bright".into()
                },
                PromptDiff {
                    op: DiffOperation::Insert,
                    text: "vivid".into()
                },
                PromptDiff {
                    op: DiffOperation::Equal,
                    text: " red ".into()
                },
                PromptDiff {
                    op: DiffOperation::Delete,
                    text: "cat".into()
                },
                PromptDiff {
                    op: DiffOperation::Insert,
                    text: "fox".into()
                },
            ]
        );
        assert_eq!(
            diff.iter()
                .filter(|part| part.op != DiffOperation::Insert)
                .map(|part| part.text.as_str())
                .collect::<String>(),
            original
        );
        assert_eq!(
            diff.iter()
                .filter(|part| part.op != DiffOperation::Delete)
                .map(|part| part.text.as_str())
                .collect::<String>(),
            proposed
        );
    }

    #[test]
    fn diff_handles_empty_and_repeated_words() {
        assert_eq!(
            word_diff("", "new prompt"),
            vec![PromptDiff {
                op: DiffOperation::Insert,
                text: "new prompt".into()
            }]
        );
        let diff = word_diff("echo echo", "echo");
        assert_eq!(
            diff,
            vec![
                PromptDiff {
                    op: DiffOperation::Equal,
                    text: "echo".into()
                },
                PromptDiff {
                    op: DiffOperation::Delete,
                    text: " echo".into()
                },
            ]
        );
    }
}
