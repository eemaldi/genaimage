//! Workflow import: detect a ComfyUI API-format graph, propose a manifest for
//! it, and write a confirmed workflow into the user workflow root.
//!
//! Detection is deterministic and read-only. Nothing is guessed: nodes without
//! an import rule are listed, and inputs that cannot be bound are reported with
//! the reason. Confirmation re-validates everything the client sends with the
//! discovery rules and never overwrites an existing workflow.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::Serialize;
use serde_json::{Map, Number, Value};
use uuid::Uuid;

use crate::{
    safe_file,
    workflow::{
        is_reserved_workflow_id, sha256_hex, workflow_revision, OutputDefinition,
        ParameterDefinition, ParameterKind, ValidationIssue, WorkflowManifest, MANIFEST_VERSION,
    },
    Harness, HarnessError, LoadedWorkflow,
};

/// Fixed names inside `<paths.workflows>/<id>/`; clients never choose them.
pub const GRAPH_FILE: &str = "api-workflow.json";
pub const MANIFEST_FILE: &str = "workflow.genaimage.json";
pub const MAX_GRAPH_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_NODES: usize = 1000;
pub const MAX_INPUTS_PER_NODE: usize = 256;
pub const MAX_TOTAL_INPUTS: usize = 20_000;
pub const MAX_ID_LENGTH: usize = 64;
const MAX_ISSUES: usize = 20;
const MAX_TITLE_CHARS: usize = 100;
const DEFAULT_SLUG: &str = "imported-workflow";

/// Serialises confirmations within this process only; other processes are
/// kept apart by the no-clobber links and the rollback rule, not by this lock.
static CONFIRM_LOCK: Mutex<()> = Mutex::new(());

/// One problem with the submitted graph or manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImportIssue {
    pub field: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("the file is a ComfyUI UI-format export, not an API-format workflow")]
    UiFormat,
    #[error("the graph is not a valid ComfyUI API-format workflow: {}", join_issues(.0))]
    InvalidGraph(Vec<ImportIssue>),
    #[error("the graph is {size} bytes once normalised; the import limit is {limit} bytes")]
    GraphTooLarge { size: usize, limit: usize },
    #[error("graph_sha256 does not match the submitted graph; the graph differs from the one that was proposed (expected {expected})")]
    GraphMismatch { expected: String },
    #[error("the manifest is invalid: {}", join_issues(.0))]
    InvalidManifest(Vec<ImportIssue>),
    #[error("workflow id '{id}' already exists; nothing was written. Use '{suggested_id}' or another unused id")]
    Exists { id: String, suggested_id: String },
    #[error(transparent)]
    Harness(#[from] HarnessError),
}

impl From<io::Error> for ImportError {
    fn from(error: io::Error) -> Self {
        Self::Harness(error.into())
    }
}

fn join_issues(issues: &[ImportIssue]) -> String {
    issues
        .iter()
        .map(|issue| format!("{}: {}", issue.field, issue.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A node class with import rules and the manifest parameters it received.
#[derive(Clone, Debug, Serialize)]
pub struct RecognisedNode {
    pub node: String,
    pub class_type: String,
    pub bound_inputs: Vec<BoundInput>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundInput {
    pub input: String,
    pub parameter: String,
}

/// A node without import rules. Its inputs keep the workflow values.
#[derive(Clone, Debug, Serialize)]
pub struct UnknownNode {
    pub node: String,
    pub class_type: String,
}

/// An input of a recognised node that no parameter controls.
#[derive(Clone, Debug, Serialize)]
pub struct UnboundInput {
    pub node: String,
    pub class_type: String,
    pub input: String,
    /// `true` when the input is a connection to another node's output.
    pub linked: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportProposal {
    /// SHA-256 of the normalised graph bytes the confirmation will write.
    pub graph_sha256: String,
    pub manifest: WorkflowManifest,
    pub recognised: Vec<RecognisedNode>,
    pub unknown_nodes: Vec<UnknownNode>,
    pub unbound_inputs: Vec<UnboundInput>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ImportedWorkflow {
    pub id: String,
    pub revision: String,
    pub manifest_path: PathBuf,
}

/// Normalised graph bytes: keys sorted at every level, compact, one trailing
/// newline. Numbers keep their exact source text (`2.0`, `-0.0`, large seeds).
pub fn canonical_graph_bytes(graph: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&sorted(graph)).expect("a JSON value always serialises");
    bytes.push(b'\n');
    bytes
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let ordered: BTreeMap<&String, Value> = map
                .iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            Value::Object(
                ordered
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Bytes written for a manifest: absent optional fields omitted, pretty, one trailing newline.
pub fn manifest_bytes(manifest: &WorkflowManifest) -> Vec<u8> {
    let mut value = serde_json::to_value(manifest).expect("a manifest always serialises");
    if let Some(top) = value.as_object_mut() {
        top.retain(|_, field| !field.is_null());
        if let Some(parameters) = top.get_mut("parameters").and_then(Value::as_object_mut) {
            for parameter in parameters.values_mut().filter_map(Value::as_object_mut) {
                parameter.retain(|_, field| !field.is_null());
            }
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&sorted(&value)).expect("a value serialises");
    bytes.push(b'\n');
    bytes
}

fn issue(field: impl Into<String>, reason: impl Into<String>) -> ImportIssue {
    ImportIssue {
        field: field.into(),
        reason: reason.into(),
    }
}

fn node_order(id: &str) -> (u8, u64, &str) {
    match id.parse::<u64>() {
        Ok(number) => (0, number, id),
        Err(_) => (1, 0, id),
    }
}

/// Checks the graph is an API-format workflow and within the import bounds.
fn check_graph(graph: &Value) -> Result<&Map<String, Value>, ImportError> {
    let Some(nodes) = graph.as_object() else {
        return Err(ImportError::InvalidGraph(vec![issue(
            "graph",
            "expected a JSON object keyed by node ID",
        )]));
    };
    if nodes.get("nodes").is_some_and(Value::is_array)
        || nodes.get("links").is_some_and(Value::is_array)
    {
        return Err(ImportError::UiFormat);
    }
    let mut issues = Vec::new();
    if nodes.is_empty() {
        issues.push(issue("graph", "the workflow contains no nodes"));
    }
    if nodes.len() > MAX_NODES {
        issues.push(issue(
            "graph",
            format!(
                "the workflow has {} nodes; the import limit is {MAX_NODES}",
                nodes.len()
            ),
        ));
    }
    let mut total_inputs = 0usize;
    for (id, node) in nodes.iter().take(MAX_NODES) {
        let mut problem = None;
        if id.is_empty() || id.len() > MAX_ID_LENGTH {
            problem = Some(format!(
                "node IDs must be 1 to {MAX_ID_LENGTH} characters long"
            ));
        } else if id == "nodes" || id == "links" {
            problem = Some(format!(
                "'{id}' cannot be a node ID because ComfyUI UI-format exports use it as a top-level key; export the workflow in API format, where node IDs are numbers"
            ));
        } else if let Some(node) = node.as_object() {
            match (
                node.get("class_type").and_then(Value::as_str),
                node.get("inputs").and_then(Value::as_object),
            ) {
                (Some(class), Some(inputs)) if !class.is_empty() => {
                    total_inputs += inputs.len();
                    if inputs.len() > MAX_INPUTS_PER_NODE {
                        problem = Some(format!(
                            "the node has {} inputs; the import limit is {MAX_INPUTS_PER_NODE} per node",
                            inputs.len()
                        ));
                    }
                }
                (class, inputs) => {
                    problem = Some(format!(
                        "an API-format node needs a non-empty string 'class_type' and an object 'inputs' (found {}{})",
                        if class.is_some_and(|class| !class.is_empty()) { "" } else { "no usable class_type" },
                        if inputs.is_some() { "" } else if class.is_some_and(|class| !class.is_empty()) { "no inputs object" } else { " and no inputs object" },
                    ));
                }
            }
        } else {
            problem = Some("expected an object with 'class_type' and 'inputs'".to_owned());
        }
        if let Some(reason) = problem {
            issues.push(issue(format!("graph.{id}"), reason));
        }
        if issues.len() >= MAX_ISSUES {
            break;
        }
    }
    if issues.is_empty() && total_inputs > MAX_TOTAL_INPUTS {
        issues.push(issue(
            "graph",
            format!(
                "the workflow has {total_inputs} inputs; the import limit is {MAX_TOTAL_INPUTS}"
            ),
        ));
    }
    if issues.is_empty() {
        Ok(nodes)
    } else {
        issues.truncate(MAX_ISSUES);
        Err(ImportError::InvalidGraph(issues))
    }
}

struct InputRule {
    input: &'static str,
    base: &'static str,
    kind: ParameterKind,
    widget: Option<&'static str>,
    minimum: Option<i64>,
    maximum: Option<i64>,
    step: Option<f64>,
    group: &'static str,
    what: &'static str,
}

const fn rule(
    input: &'static str,
    base: &'static str,
    kind: ParameterKind,
    group: &'static str,
    what: &'static str,
) -> InputRule {
    InputRule {
        input,
        base,
        kind,
        widget: None,
        minimum: None,
        maximum: None,
        step: None,
        group,
        what,
    }
}

const fn bounded(mut rule: InputRule, minimum: i64, maximum: i64) -> InputRule {
    rule.minimum = Some(minimum);
    rule.maximum = Some(maximum);
    rule
}

/// Import rules per node class. Parameters are only ever created from these.
fn rules(class: &str) -> Option<Vec<InputRule>> {
    use ParameterKind::{ImageUpload, Integer, Number, Seed, String};
    let canvas = |input, base| {
        let mut size = bounded(
            rule(input, base, Integer, "Canvas", "Pixel size"),
            16,
            16384,
        );
        size.step = Some(8.0);
        size
    };
    Some(match class {
        "CheckpointLoaderSimple" => vec![rule(
            "ckpt_name",
            "checkpoint",
            String,
            "Model",
            "Checkpoint file name",
        )],
        "CLIPTextEncode" => {
            let mut text = rule("text", "text", String, "Prompt", "Prompt text");
            text.widget = Some("textarea");
            vec![text]
        }
        "KSampler" => {
            let mut steps = rule("steps", "steps", Integer, "Sampler", "Sampling steps");
            steps.minimum = Some(1);
            let mut denoise = rule("denoise", "denoise", Number, "Sampler", "Denoise strength");
            denoise.minimum = Some(0);
            denoise.maximum = Some(1);
            vec![
                rule("seed", "seed", Seed, "Sampler", "Sampler seed"),
                steps,
                rule(
                    "cfg",
                    "cfg",
                    Number,
                    "Sampler",
                    "Classifier-free guidance scale",
                ),
                rule(
                    "sampler_name",
                    "sampler_name",
                    String,
                    "Sampler",
                    "Sampler name",
                ),
                rule(
                    "scheduler",
                    "scheduler",
                    String,
                    "Sampler",
                    "Scheduler name",
                ),
                denoise,
            ]
        }
        "EmptyLatentImage" => vec![
            canvas("width", "width"),
            canvas("height", "height"),
            bounded(
                rule(
                    "batch_size",
                    "batch_size",
                    Integer,
                    "Canvas",
                    "Images per batch",
                ),
                1,
                4096,
            ),
        ],
        "LoadImage" => {
            let mut image = rule("image", "image", ImageUpload, "Image", "Reference image");
            image.widget = Some("file");
            vec![image]
        }
        "LoraLoader" | "LoraLoaderModelOnly" => {
            let strength =
                |input, base, what| bounded(rule(input, base, Number, "LoRA", what), -100, 100);
            vec![
                rule("lora_name", "lora_name", String, "LoRA", "LoRA file name"),
                strength(
                    "strength_model",
                    "lora_strength_model",
                    "LoRA model strength",
                ),
                strength("strength_clip", "lora_strength_clip", "LoRA CLIP strength"),
            ]
        }
        _ => return None,
    })
}

fn is_recognised_class(class: &str) -> bool {
    class == "SaveImage" || rules(class).is_some()
}

fn link_target(value: &Value) -> Option<(String, u64)> {
    let [node, output] = value.as_array()?.as_slice() else {
        return None;
    };
    let node = match node {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.as_u64()?.to_string(),
        _ => return None,
    };
    Some((node, output.as_u64()?))
}

#[derive(PartialEq)]
enum Role {
    Positive,
    Negative,
}

/// Which `CLIPTextEncode` nodes a `KSampler` consumes directly as positive or negative.
fn trace_roles(nodes: &Map<String, Value>) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut roles: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for node in nodes.values() {
        if node["class_type"] != "KSampler" {
            continue;
        }
        for (input, role) in [("positive", Role::Positive), ("negative", Role::Negative)] {
            let Some((target, _)) = node["inputs"].get(input).and_then(link_target) else {
                continue;
            };
            if nodes
                .get(&target)
                .is_some_and(|node| node["class_type"] == "CLIPTextEncode")
            {
                roles
                    .entry(target)
                    .or_default()
                    .insert(if role == Role::Positive {
                        "prompt"
                    } else {
                        "negative_prompt"
                    });
            }
        }
    }
    roles
}

struct Candidate {
    base: String,
    node: String,
    input: &'static str,
    definition: ParameterDefinition,
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        DEFAULT_SLUG.to_owned()
    } else if is_reserved_workflow_id(&slug) {
        format!("{slug}-workflow")
    } else {
        slug
    }
}

/// A safe directory name: lowercase letters, digits, `-` and `_`, starting
/// with a letter or digit, at most [`MAX_ID_LENGTH`] characters.
pub fn is_safe_workflow_id(id: &str) -> bool {
    let mut characters = id.chars();
    id.len() <= MAX_ID_LENGTH
        && characters
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Proposes a manifest for an API-format graph. Pure and deterministic: the
/// same graph and name always give the same proposal.
pub fn propose(graph: &Value, name: Option<&str>) -> Result<ImportProposal, ImportError> {
    check_graph(graph)?;
    let canonical = canonical_graph_bytes(graph);
    if canonical.len() > MAX_GRAPH_BYTES {
        return Err(ImportError::GraphTooLarge {
            size: canonical.len(),
            limit: MAX_GRAPH_BYTES,
        });
    }
    let graph = sorted(graph);
    let nodes = graph.as_object().expect("check_graph accepted an object");
    let mut ordered: Vec<(&String, &Map<String, Value>, &str)> = nodes
        .iter()
        .map(|(id, node)| {
            (
                id,
                node["inputs"].as_object().expect("checked inputs object"),
                node["class_type"].as_str().expect("checked class_type"),
            )
        })
        .collect();
    ordered.sort_by(|left, right| node_order(left.0).cmp(&node_order(right.0)));

    let roles = trace_roles(nodes);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut failed: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut unknown_nodes = Vec::new();
    let mut outputs = Vec::new();

    for &(id, inputs, class) in &ordered {
        if class == "SaveImage" {
            outputs.push(OutputDefinition {
                node: id.clone(),
                kind: "image".to_owned(),
            });
            continue;
        }
        let Some(class_rules) = rules(class) else {
            unknown_nodes.push(UnknownNode {
                node: id.clone(),
                class_type: class.to_owned(),
            });
            continue;
        };
        for rule in class_rules {
            let Some(value) = inputs.get(rule.input) else {
                continue;
            };
            if value.is_array() {
                continue;
            }
            let mut base = rule.base.to_owned();
            if class == "CLIPTextEncode" {
                base = match roles
                    .get(id.as_str())
                    .map(|set| set.iter().copied().collect::<Vec<_>>())
                    .as_deref()
                {
                    Some(["negative_prompt"]) => "negative_prompt".to_owned(),
                    Some(["prompt"]) => "prompt".to_owned(),
                    Some(_) => {
                        warnings.push(format!(
                            "CLIPTextEncode node '{id}' feeds both a positive and a negative input; it is named 'text_{id}'. Rename it before confirming."
                        ));
                        format!("text_{id}")
                    }
                    None => {
                        warnings.push(format!(
                            "Could not tell whether CLIPTextEncode node '{id}' is a positive or negative prompt, because no KSampler positive or negative input links to it directly; it is named 'text_{id}'. Rename it before confirming."
                        ));
                        format!("text_{id}")
                    }
                };
            }
            let imageful = rule.kind == ParameterKind::ImageUpload;
            let definition = ParameterDefinition {
                node: id.clone(),
                input: rule.input.to_owned(),
                kind: rule.kind,
                required: imageful,
                description: format!("{} for node {id} ({class}).", rule.what),
                default: (!imageful).then(|| value.clone()),
                allowed_values: None,
                minimum: rule.minimum.map(Number::from),
                maximum: rule.maximum.map(Number::from),
                widget: rule.widget.map(str::to_owned),
                step: rule.step,
                options: None,
                group: Some(rule.group.to_owned()),
                advanced: None,
                placeholder: None,
            };
            let check = if imageful {
                definition.validate_value(rule.input, value)
            } else {
                definition.default.as_ref().map_or(Ok(()), |default| {
                    definition.validate_value(rule.input, default)
                })
            };
            match check {
                Ok(()) => {
                    if imageful {
                        warnings.push(format!(
                            "LoadImage node '{id}' names the ComfyUI-side file {value}; it is not used as a default. Supply an upload for every run."
                        ));
                    }
                    candidates.push(Candidate {
                        base,
                        node: id.clone(),
                        input: rule.input,
                        definition,
                    });
                }
                Err(error) => {
                    failed.insert((id.clone(), rule.input.to_owned()), error.to_string());
                }
            }
        }
    }

    let mut base_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for candidate in &candidates {
        *base_counts.entry(candidate.base.as_str()).or_default() += 1;
    }
    let suffixed: BTreeSet<String> = base_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(base, _)| base.to_owned())
        .collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut parameters = BTreeMap::new();
    let mut bound: BTreeMap<(String, String), String> = BTreeMap::new();
    for candidate in candidates {
        let mut name = if suffixed.contains(&candidate.base) {
            format!("{}_{}", candidate.base, candidate.node)
        } else {
            candidate.base.clone()
        };
        let mut attempt = 2;
        while used.contains(&name) {
            name = format!("{}_{}_{attempt}", candidate.base, candidate.node);
            attempt += 1;
        }
        used.insert(name.clone());
        bound.insert(
            (candidate.node.clone(), candidate.input.to_owned()),
            name.clone(),
        );
        parameters.insert(name, candidate.definition);
    }

    let mut recognised = Vec::new();
    let mut unbound_inputs = Vec::new();
    for &(id, inputs, class) in &ordered {
        if !is_recognised_class(class) {
            continue;
        }
        let mut bound_inputs = Vec::new();
        for (input, value) in inputs {
            let key = (id.clone(), input.clone());
            if let Some(parameter) = bound.get(&key) {
                bound_inputs.push(BoundInput {
                    input: input.clone(),
                    parameter: parameter.clone(),
                });
                continue;
            }
            let (linked, reason) = match (link_target(value), failed.get(&key)) {
                (Some((node, output)), _) => (
                    true,
                    format!("connected to output {output} of node '{node}'; connections cannot be bound"),
                ),
                (None, _) if value.is_array() => (
                    false,
                    "array values cannot be bound".to_owned(),
                ),
                (None, Some(reason)) => (false, format!("the workflow value is not accepted: {reason}")),
                (None, None) => (
                    false,
                    "no import rule for this input; it keeps the workflow value".to_owned(),
                ),
            };
            unbound_inputs.push(UnboundInput {
                node: id.clone(),
                class_type: class.to_owned(),
                input: input.clone(),
                linked,
                reason,
            });
        }
        recognised.push(RecognisedNode {
            node: id.clone(),
            class_type: class.to_owned(),
            bound_inputs,
        });
    }

    if outputs.is_empty() {
        warnings.push(
            "The workflow has no SaveImage node, so the proposal declares no outputs.".to_owned(),
        );
    }
    let title: String = name
        .map(|name| {
            name.trim()
                .chars()
                .take(MAX_TITLE_CHARS)
                .collect::<String>()
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Imported workflow".to_owned());
    let manifest = WorkflowManifest {
        schema_version: MANIFEST_VERSION,
        id: slugify(name.unwrap_or(DEFAULT_SLUG)),
        version: "1".to_owned(),
        title,
        workflow: PathBuf::from(GRAPH_FILE),
        description: None,
        tags: None,
        category: None,
        thumbnail: None,
        parameters,
        presets: BTreeMap::new(),
        outputs,
    };
    let (_, problems) = LoadedWorkflow::from_bytes_unchecked(
        manifest.clone(),
        manifest_bytes(&manifest),
        &canonical,
        Path::new("."),
    )?;
    if !problems.is_empty() {
        return Err(HarnessError::Workflow(format!(
            "internal error: the proposed manifest failed validation: {}",
            problems
                .iter()
                .map(|problem| format!("{}: {}", problem.field, problem.message))
                .collect::<Vec<_>>()
                .join("; ")
        ))
        .into());
    }
    Ok(ImportProposal {
        graph_sha256: sha256_hex(&canonical),
        manifest,
        recognised,
        unknown_nodes,
        unbound_inputs,
        warnings,
    })
}

/// Keeps the first [`MAX_ISSUES`] issues and says how many were left out.
fn cap_issues(mut issues: Vec<ImportIssue>) -> Vec<ImportIssue> {
    if issues.len() > MAX_ISSUES {
        let more = issues.len() - MAX_ISSUES;
        issues.truncate(MAX_ISSUES);
        issues.push(issue("manifest", format!("and {more} more")));
    }
    issues
}

fn manifest_issues(issues: Vec<ValidationIssue>) -> Vec<ImportIssue> {
    cap_issues(
        issues
            .into_iter()
            .map(|item| issue(item.field, item.message))
            .collect(),
    )
}

/// Strictly validates a (possibly edited) manifest the way discovery would,
/// plus the rules that make an import safe to write.
fn validate_manifest(
    manifest: &Value,
    canonical: &[u8],
) -> Result<(WorkflowManifest, Vec<u8>), ImportError> {
    let parsed: WorkflowManifest = serde_json::from_value(manifest.clone()).map_err(|error| {
        ImportError::InvalidManifest(vec![issue("manifest", error.to_string())])
    })?;
    let mut issues = Vec::new();
    if parsed.schema_version != MANIFEST_VERSION {
        issues.push(issue(
            "schema_version",
            format!("expected schema version {MANIFEST_VERSION}"),
        ));
    }
    if is_reserved_workflow_id(&parsed.id) {
        issues.push(issue(
            "id",
            format!(
                "'{}' is reserved for a URL path; choose another id",
                parsed.id
            ),
        ));
    } else if !is_safe_workflow_id(&parsed.id) {
        issues.push(issue(
            "id",
            format!(
                "ids use lowercase letters, digits, '-' and '_', start with a letter or digit, and have at most {MAX_ID_LENGTH} characters"
            ),
        ));
    }
    if parsed.version.trim().is_empty() {
        issues.push(issue("version", "the version cannot be empty"));
    }
    if parsed.workflow != Path::new(GRAPH_FILE) {
        issues.push(issue(
            "workflow",
            format!("must be '{GRAPH_FILE}'; the server chooses the file name"),
        ));
    }
    if parsed.thumbnail.is_some() {
        issues.push(issue(
            "thumbnail",
            "thumbnails are not supported for imported workflows",
        ));
    }
    let bytes = manifest_bytes(&parsed);
    if bytes.len() > MAX_MANIFEST_BYTES {
        issues.push(issue(
            "manifest",
            format!("the manifest exceeds {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    if issues.is_empty() {
        let (_, problems) = LoadedWorkflow::from_bytes_unchecked(
            parsed.clone(),
            bytes.clone(),
            canonical,
            Path::new("."),
        )?;
        issues.extend(manifest_issues(problems));
    }
    if issues.is_empty() {
        Ok((parsed, bytes))
    } else {
        Err(ImportError::InvalidManifest(issues))
    }
}

fn read_limited(mut file: fs::File, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Creates `directory/name` atomically and without clobbering: the content is
/// written and synced to a temporary file, then hard-linked into place. A
/// failure other than "already exists" names the hard-link requirement.
fn publish_new(
    directory: &Path,
    name: &str,
    bytes: &[u8],
    created: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let temporary = directory.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    created.push(temporary.clone());
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let target = directory.join(name);
    let linked = fs::hard_link(&temporary, &target);
    let _ = fs::remove_file(&temporary);
    linked.map_err(|error| link_error(error, &target))?;
    created.push(target);
    Ok(())
}

fn link_error(error: io::Error, target: &Path) -> io::Error {
    if error.kind() == io::ErrorKind::AlreadyExists {
        return error;
    }
    io::Error::new(
        error.kind(),
        format!(
            "cannot create '{}' with a hard link ({error}); importing needs hard links, so paths.workflows must be on a filesystem that supports them",
            target.display()
        ),
    )
}

fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

/// Creates `root` and any missing parents, then syncs the parent of every
/// directory that was created so the new entries survive a crash.
fn create_root(root: &Path) -> io::Result<()> {
    let missing: Vec<&Path> = root
        .ancestors()
        .take_while(|path| !path.as_os_str().is_empty() && fs::symlink_metadata(path).is_err())
        .collect();
    fs::create_dir_all(root)?;
    for path in missing.iter().rev() {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

/// Removes only the files this call created, and the directory only when this
/// call created it and nothing else is in it.
fn rollback(directory: &Path, created: &[PathBuf], remove_directory: bool) {
    for path in created.iter().rev() {
        let _ = fs::remove_file(path);
    }
    if remove_directory {
        let _ = fs::remove_dir(directory);
    }
}

/// Publishes the graph, syncs, publishes the manifest, syncs: the manifest is
/// the last file to appear, so no half-written workflow is discoverable.
fn write_workflow(
    directory: &Path,
    graph: Option<&[u8]>,
    manifest: &[u8],
    created: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if let Some(graph) = graph {
        publish_new(directory, GRAPH_FILE, graph, created)?;
        sync_directory(directory)?;
    }
    publish_new(directory, MANIFEST_FILE, manifest, created)?;
    sync_directory(directory)
}

const MAX_LEFTOVER_ENTRIES: usize = 64;

/// `.<file>.<uuid>.tmp`, the temporary name [`publish_new`] uses.
fn is_stale_temporary(name: &str) -> bool {
    [GRAPH_FILE, MANIFEST_FILE].iter().any(|file| {
        name.strip_prefix('.')
            .and_then(|rest| rest.strip_prefix(file))
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .is_some_and(|id| Uuid::parse_str(id).is_ok())
    })
}

/// A crash between publishing the graph and the manifest leaves the graph and
/// maybe stale temporary files, nothing else.
fn holds_only_graph_and_temporaries(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut seen_graph = false;
    for (index, entry) in entries.enumerate() {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        let regular = fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.is_file());
        if index >= MAX_LEFTOVER_ENTRIES || !regular {
            return false;
        }
        if name == GRAPH_FILE {
            seen_graph = true;
        } else if !is_stale_temporary(name) {
            return false;
        }
    }
    seen_graph
}

fn holds_identical(root: &Path, directory: &Path, name: &str, expected: &[u8]) -> bool {
    safe_file::open_regular_beneath(root, directory, Path::new(name))
        .and_then(|file| read_limited(file, expected.len()).ok())
        .is_some_and(|bytes| bytes == expected)
}

impl Harness {
    /// Every workflow ID claimed by the user root or any pack, including
    /// invalid and duplicate claimants. Fails when a directory cannot be
    /// inspected, because uniqueness could then not be proven.
    fn taken_workflow_ids(&self) -> Result<BTreeSet<String>, HarnessError> {
        let discovery = self.discover_workflows()?;
        if let Some(failure) = discovery.traversal_failures.first() {
            return Err(HarnessError::Workflow(format!(
                "cannot verify that the workflow id is unused: '{}' could not be inspected ({}); fix its permissions and retry",
                failure.path.display(),
                failure.reason
            )));
        }
        let mut ids: BTreeSet<String> = discovery
            .workflows
            .iter()
            .map(|workflow| workflow.manifest.id.clone())
            .collect();
        ids.extend(discovery.unresolvable_ids);
        Ok(ids)
    }

    fn free_workflow_id(&self, base: &str, taken: &BTreeSet<String>) -> String {
        let root = &self.config().workflows;
        let is_free = |id: &str| {
            !taken.contains(id)
                && matches!(
                    fs::symlink_metadata(root.join(id)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound
                )
        };
        if is_free(base) {
            return base.to_owned();
        }
        (2..10_000)
            .map(|n| {
                let suffix = format!("-{n}");
                let stem: String = base.chars().take(MAX_ID_LENGTH - suffix.len()).collect();
                format!("{stem}{suffix}")
            })
            .find(|candidate| is_free(candidate))
            .unwrap_or_else(|| format!("imported-{}", Uuid::new_v4().simple()))
    }

    /// Detects and proposes. Reads the workflow catalogue only to suggest an
    /// unused id; writes nothing.
    pub fn propose_workflow_import(
        &self,
        graph: &Value,
        name: Option<&str>,
    ) -> Result<ImportProposal, ImportError> {
        let mut proposal = propose(graph, name)?;
        match self.taken_workflow_ids() {
            Ok(taken) => {
                proposal.manifest.id = self.free_workflow_id(&proposal.manifest.id, &taken);
            }
            Err(error) => proposal.warnings.push(format!(
                "The suggested id '{}' could not be checked against existing workflows: {error}",
                proposal.manifest.id
            )),
        }
        Ok(proposal)
    }

    /// Writes a confirmed import into a new `<paths.workflows>/<id>/` directory.
    /// Never overwrites. An identical repeat of an earlier confirmation succeeds
    /// again with the same result, and a directory that a crash left holding
    /// only the identical graph is completed by publishing the manifest.
    pub fn confirm_workflow_import(
        &self,
        graph: &Value,
        graph_sha256: &str,
        manifest: &Value,
    ) -> Result<ImportedWorkflow, ImportError> {
        check_graph(graph)?;
        let canonical = canonical_graph_bytes(graph);
        if canonical.len() > MAX_GRAPH_BYTES {
            return Err(ImportError::GraphTooLarge {
                size: canonical.len(),
                limit: MAX_GRAPH_BYTES,
            });
        }
        let expected = sha256_hex(&canonical);
        if !expected.eq_ignore_ascii_case(graph_sha256) {
            return Err(ImportError::GraphMismatch { expected });
        }
        let (parsed, manifest_bytes) = validate_manifest(manifest, &canonical)?;

        let _serialised = CONFIRM_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = self.config().workflows.clone();
        let directory = root.join(&parsed.id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => {
                if holds_identical(&root, &directory, GRAPH_FILE, &canonical)
                    && holds_identical(&root, &directory, MANIFEST_FILE, &manifest_bytes)
                {
                    return self.imported(&parsed.id, &directory);
                }
                if !holds_identical(&root, &directory, GRAPH_FILE, &canonical)
                    || !holds_only_graph_and_temporaries(&directory)
                {
                    return Err(self.exists(&parsed.id, &self.taken_workflow_ids()?));
                }
                let taken = self.taken_workflow_ids()?;
                if taken.contains(&parsed.id) {
                    return Err(self.exists(&parsed.id, &taken));
                }
                return self.publish_manifest(&root, &directory, &parsed.id, &manifest_bytes);
            }
            Ok(_) => return Err(self.exists(&parsed.id, &self.taken_workflow_ids()?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let taken = self.taken_workflow_ids()?;
        if taken.contains(&parsed.id) {
            return Err(self.exists(&parsed.id, &taken));
        }

        create_root(&root)?;
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(self.exists(&parsed.id, &taken));
            }
            Err(error) => return Err(error.into()),
        }
        let mut created = Vec::new();
        if let Err(error) = publish_new(&directory, GRAPH_FILE, &canonical, &mut created) {
            // Nothing was linked, so only our temporary file and the empty
            // directory we just created are removed.
            rollback(&directory, &created, true);
            return Err(error.into());
        }
        // From here the graph stays whatever happens: another call may already
        // have adopted it, and an identical retry completes a leftover.
        sync_directory(&directory)?;
        sync_directory(&root)?;
        self.publish_manifest(&root, &directory, &parsed.id, &manifest_bytes)
    }

    fn exists(&self, id: &str, taken: &BTreeSet<String>) -> ImportError {
        ImportError::Exists {
            id: id.to_owned(),
            suggested_id: self.free_workflow_id(id, taken),
        }
    }

    /// Links the manifest into a directory that already holds the identical
    /// graph. `AlreadyExists` means another call won the race: the same
    /// manifest replays as success, a different one is `workflow-exists`, and
    /// nothing is removed either way. Only a manifest this call linked is rolled
    /// back, never the graph or the directory.
    fn publish_manifest(
        &self,
        root: &Path,
        directory: &Path,
        id: &str,
        manifest_bytes: &[u8],
    ) -> Result<ImportedWorkflow, ImportError> {
        let mut created = Vec::new();
        match write_workflow(directory, None, manifest_bytes, &mut created) {
            Ok(()) => self.imported(id, directory).inspect_err(|_| {
                rollback(directory, &created, false);
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                rollback(directory, &created, false);
                if holds_identical(root, directory, MANIFEST_FILE, manifest_bytes) {
                    self.imported(id, directory)
                } else {
                    Err(self.exists(id, &self.taken_workflow_ids()?))
                }
            }
            Err(error) => {
                rollback(directory, &created, false);
                Err(error.into())
            }
        }
    }

    /// Loads what is on disk with the discovery loader, so a reported success
    /// is a workflow that can be loaded.
    fn imported(&self, id: &str, directory: &Path) -> Result<ImportedWorkflow, ImportError> {
        let manifest_path = directory.join(MANIFEST_FILE);
        let loaded = LoadedWorkflow::load(&manifest_path)?;
        if loaded.manifest.id != id {
            return Err(HarnessError::Workflow(format!(
                "'{}' declares id '{}', not '{id}'",
                manifest_path.display(),
                loaded.manifest.id
            ))
            .into());
        }
        Ok(ImportedWorkflow {
            id: id.to_owned(),
            revision: workflow_revision(&loaded.workflow_sha256, &loaded.manifest_sha256),
            manifest_path,
        })
    }
}

#[cfg(test)]
mod tests;
