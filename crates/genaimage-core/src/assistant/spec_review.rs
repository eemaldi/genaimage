//! Review of an output against one exact character spec version.
//!
//! The model's checklist is an opinion. It is validated against the spec
//! snapshot the model was shown and never coerced: unknown ids, invalid
//! verdicts and oversized notes are dropped with a note, and a mandatory
//! element the model did not assess is recorded as `unclear`.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    assistant::PromptTemplates,
    characters::{CharacterSpec, CharacterSpecVersion, ElementCategory, Requirement, SpecElement},
    HarnessError, Result,
};

pub const SPEC_REVIEW_KIND: &str = "spec_review";
pub const SPEC_REVIEW_PROMPT_TEMPLATE: &str = "spec_review";
pub const SPEC_REVIEW_NOTE_MAX_BYTES: usize = 500;
pub const SPEC_REVIEW_SUMMARY_MAX_BYTES: usize = 1_000;
pub const SPEC_REVIEW_LABEL: &str =
    "Model opinion, not approval. Record an approval or rejection separately.";
pub const NOT_ASSESSED_NOTE: &str = "not assessed by the model";
pub const CONFLICTING_DUPLICATE_NOTE: &str = "conflicting duplicate entries; not recorded";
pub const SPEC_REVIEW_DROPPED_MAX: usize = 50;
pub const SPEC_REVIEW_UNKNOWN_IDS_LISTED: usize = 20;
const ELEMENTS_PER_REVIEWED_ELEMENT: usize = 4;
const ELEMENTS_SLACK: usize = 16;

const DEFAULT_SPEC_REVIEW_PROMPT: &str = r#"Review the supplied generated image against the character specification below.

Assess every element in the specification, using its "id" exactly as given. Return only one strict JSON object with exactly these fields:
- "elements": an array with one entry per assessed element, each {"id": "<element id>", "verdict": "met" | "deviation" | "unclear", "note": "<short factual note>"}
- "summary": a short factual summary of how well the image matches the specification

Use "unclear" when the image does not let you decide. Mandatory elements must each appear exactly once; flexible elements may be left out. Do not use Markdown, add fields or invent ids. The response is a model opinion, not an approval.

Character specification:
<character_spec>
{{character_spec}}
</character_spec>
"#;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecVerdict {
    Met,
    Deviation,
    Unclear,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpecReviewItem {
    pub id: String,
    pub category: ElementCategory,
    pub requirement: Requirement,
    pub verdict: SpecVerdict,
    pub note: String,
}

/// The validated checklist stored with a `spec_review` annotation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpecReview {
    pub character_id: String,
    pub spec_version: u32,
    /// Mandatory elements first, then the assessed flexible elements, each in spec order.
    pub checklist: Vec<SpecReviewItem>,
    /// One note per dropped model item or id; empty when nothing was dropped.
    pub dropped: Vec<String>,
    pub summary: String,
    pub label: String,
}

fn response_error(message: impl Into<String>) -> HarnessError {
    HarnessError::VisionReviewResponse(message.into())
}

pub(super) async fn spec_review_prompt(data_dir: &Path, spec: &CharacterSpec) -> Result<String> {
    let shown: Vec<&SpecElement> = reviewed_elements(spec);
    if shown.is_empty() {
        return Err(HarnessError::SpecReviewNothingToReview);
    }
    let rendered = serde_json::to_string_pretty(&serde_json::json!({
        "summary": spec.summary,
        "palette": spec.palette,
        "elements": shown,
    }))?;
    let mut templates = PromptTemplates::new(data_dir);
    templates.register_builtin(SPEC_REVIEW_PROMPT_TEMPLATE, DEFAULT_SPEC_REVIEW_PROMPT)?;
    let template = templates.load(SPEC_REVIEW_PROMPT_TEMPLATE).await?.content;
    if template.contains("{{character_spec}}") {
        Ok(template.replace("{{character_spec}}", &rendered))
    } else {
        Ok(format!(
            "{template}\n\nCharacter specification:\n<character_spec>\n{rendered}\n</character_spec>\n"
        ))
    }
}

/// The mandatory and flexible elements the model is shown, in spec order.
fn reviewed_elements(spec: &CharacterSpec) -> Vec<&SpecElement> {
    spec.elements
        .iter()
        .filter(|element| element.requirement != Requirement::Unspecified)
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelChecklist {
    elements: Vec<Value>,
    summary: String,
}

struct Claim {
    verdict: SpecVerdict,
    note: String,
}

pub(super) fn validate_spec_review(
    raw_response: &str,
    version: &CharacterSpecVersion,
) -> Result<SpecReview> {
    let parsed: ModelChecklist = serde_json::from_str(raw_response).map_err(|error| {
        response_error(format!(
            "the response is not the required checklist JSON (it may be truncated): {error}"
        ))
    })?;
    if parsed.summary.len() > SPEC_REVIEW_SUMMARY_MAX_BYTES {
        return Err(response_error(format!(
            "the summary is {} bytes; the limit is {SPEC_REVIEW_SUMMARY_MAX_BYTES} bytes",
            parsed.summary.len()
        )));
    }
    let shown: BTreeMap<&str, &SpecElement> = reviewed_elements(&version.spec)
        .into_iter()
        .map(|element| (element.id.as_str(), element))
        .collect();

    let element_limit = shown.len() * ELEMENTS_PER_REVIEWED_ELEMENT + ELEMENTS_SLACK;
    if parsed.elements.len() > element_limit {
        return Err(response_error(format!(
            "the checklist has {} entries for {} reviewed elements; the limit is {element_limit}; nothing was stored",
            parsed.elements.len(),
            shown.len()
        )));
    }

    let mut dropped = Vec::new();
    let mut unknown = Vec::new();
    let mut claims: BTreeMap<String, Vec<Claim>> = BTreeMap::new();
    for (index, item) in parsed.elements.iter().enumerate() {
        let Some(object) = item.as_object() else {
            dropped.push(format!("elements[{index}] dropped: not an object"));
            continue;
        };
        let Some(id) = object.get("id").and_then(Value::as_str) else {
            dropped.push(format!("elements[{index}] dropped: missing string id"));
            continue;
        };
        if !shown.contains_key(id) {
            unknown.push(id.chars().take(ELEMENT_ID_NOTE_CHARS).collect::<String>());
            continue;
        }
        if let Some(field) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "id" | "verdict" | "note"))
        {
            dropped.push(format!(
                "element '{id}' dropped: unexpected field '{}'",
                field.chars().take(48).collect::<String>()
            ));
            continue;
        }
        let verdict = match object.get("verdict").and_then(Value::as_str) {
            Some("met") => SpecVerdict::Met,
            Some("deviation") => SpecVerdict::Deviation,
            Some("unclear") => SpecVerdict::Unclear,
            Some(other) => {
                dropped.push(format!(
                    "element '{id}' dropped: invalid verdict '{}'",
                    other.chars().take(48).collect::<String>()
                ));
                continue;
            }
            None => {
                dropped.push(format!("element '{id}' dropped: missing string verdict"));
                continue;
            }
        };
        let Some(note) = object.get("note").and_then(Value::as_str) else {
            dropped.push(format!("element '{id}' dropped: missing string note"));
            continue;
        };
        if note.len() > SPEC_REVIEW_NOTE_MAX_BYTES {
            dropped.push(format!(
                "element '{id}' dropped: note is {} bytes; the limit is {SPEC_REVIEW_NOTE_MAX_BYTES} bytes",
                note.len()
            ));
            continue;
        }
        claims.entry(id.to_owned()).or_default().push(Claim {
            verdict,
            note: note.to_owned(),
        });
    }
    if !unknown.is_empty() {
        let listed = unknown
            .iter()
            .take(SPEC_REVIEW_UNKNOWN_IDS_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = unknown.len().saturating_sub(SPEC_REVIEW_UNKNOWN_IDS_LISTED);
        dropped.push(if more == 0 {
            format!("unknown element ids dropped: {listed}")
        } else {
            format!("unknown element ids dropped: {listed} and {more} more")
        });
    }

    let mut assessed = 0;
    let mut mandatory = Vec::new();
    let mut flexible = Vec::new();
    for element in shown.values().copied() {
        let mut conflicting = false;
        let claim = match claims.remove(&element.id) {
            Some(mut list) if list.len() == 1 => list.pop(),
            Some(list) => {
                conflicting = true;
                dropped.push(format!(
                    "element '{}' dropped: listed {} times",
                    element.id,
                    list.len()
                ));
                None
            }
            None => None,
        };
        if claim.is_some() {
            assessed += 1;
        }
        let item = |claim: Option<Claim>| match claim {
            Some(claim) => SpecReviewItem {
                id: element.id.clone(),
                category: element.category,
                requirement: element.requirement,
                verdict: claim.verdict,
                note: claim.note,
            },
            None => SpecReviewItem {
                id: element.id.clone(),
                category: element.category,
                requirement: element.requirement,
                verdict: SpecVerdict::Unclear,
                note: if conflicting {
                    CONFLICTING_DUPLICATE_NOTE
                } else {
                    NOT_ASSESSED_NOTE
                }
                .to_owned(),
            },
        };
        match element.requirement {
            Requirement::Mandatory => mandatory.push(item(claim)),
            _ => {
                if claim.is_some() {
                    flexible.push(item(claim));
                }
            }
        }
    }
    if assessed == 0 {
        return Err(response_error(format!(
            "the checklist assessed none of the {} reviewed elements; nothing was stored",
            shown.len()
        )));
    }
    let order: BTreeMap<&str, usize> = version
        .spec
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| (element.id.as_str(), index))
        .collect();
    mandatory.sort_by_key(|item| order[item.id.as_str()]);
    flexible.sort_by_key(|item| order[item.id.as_str()]);
    mandatory.extend(flexible);
    cap_dropped(&mut dropped);
    Ok(SpecReview {
        character_id: version.character_id.clone(),
        spec_version: version.version,
        checklist: mandatory,
        dropped,
        summary: parsed.summary,
        label: SPEC_REVIEW_LABEL.to_owned(),
    })
}

fn cap_dropped(dropped: &mut Vec<String>) {
    if dropped.len() > SPEC_REVIEW_DROPPED_MAX {
        let further = dropped.len() - SPEC_REVIEW_DROPPED_MAX;
        dropped.truncate(SPEC_REVIEW_DROPPED_MAX);
        dropped.push(format!("{further} further items dropped"));
    }
}

const ELEMENT_ID_NOTE_CHARS: usize = 64;
