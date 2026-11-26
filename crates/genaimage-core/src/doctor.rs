//! Self-service diagnostics: one bounded, read-only report over configuration,
//! ComfyUI, workflows, the optional LLM, the GPU, disk space and ports.
//!
//! Probes only contact the configured endpoints and never mutate anything. An
//! unknown outcome (a timeout, an unparsable reply) is never `ok`, and a check
//! that depends on a failed one is `skipped`. Every non-ok check carries a
//! concrete `fix`.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;
use tokio::time::timeout;
use url::Url;

use crate::{
    config::{validate_endpoint, ResolvedConfig},
    Harness, LlmModel, LlmRoles, LoadedWorkflow,
};

pub const SERVER: &str = "server";
pub const CONFIG: &str = "config";
pub const COMFYUI_REACHABLE: &str = "comfyui.reachable";
pub const COMFYUI_VERSION: &str = "comfyui.version";
pub const COMFYUI_OBJECT_INFO: &str = "comfyui.object_info";
pub const WORKFLOWS_NODES: &str = "workflows.nodes";
pub const LLM_REACHABLE: &str = "llm.reachable";
pub const LLM_TEXT: &str = "llm.text";
pub const LLM_VISION: &str = "llm.vision";
pub const GPU: &str = "gpu";
pub const DISK: &str = "disk";
pub const PORTS: &str = "ports";

/// Every check id, in report order. A report always contains each id once.
pub const CHECK_IDS: [&str; 12] = [
    SERVER,
    CONFIG,
    COMFYUI_REACHABLE,
    COMFYUI_VERSION,
    COMFYUI_OBJECT_INFO,
    WORKFLOWS_NODES,
    LLM_REACHABLE,
    LLM_TEXT,
    LLM_VISION,
    GPU,
    DISK,
    PORTS,
];

const MAX_ITEMS: usize = 20;
const MAX_CLASSES_PER_WORKFLOW: usize = 10;
const MAX_WORKFLOWS: usize = 200;
const MAX_TEXT: usize = 240;
const MIN_TARGETED_INTERRUPT: (u64, u64, u64) = (0, 3, 57);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Ok,
    Warning,
    Error,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub id: String,
    pub status: DoctorStatus,
    pub summary: String,
    /// Bounded detail lines, such as missing node classes per workflow.
    pub items: Vec<String>,
    /// Present for every check that is not `ok`.
    pub fix: Option<String>,
}

impl DoctorCheck {
    pub fn ok(id: &str, summary: impl Into<String>) -> Self {
        Self::new(id, DoctorStatus::Ok, summary, None)
    }

    pub fn warning(id: &str, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(id, DoctorStatus::Warning, summary, Some(fix.into()))
    }

    pub fn error(id: &str, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(id, DoctorStatus::Error, summary, Some(fix.into()))
    }

    pub fn skipped(id: &str, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(id, DoctorStatus::Skipped, summary, Some(fix.into()))
    }

    pub fn with_items(mut self, items: Vec<String>) -> Self {
        self.items = cap_items(items);
        self
    }

    fn new(
        id: &str,
        status: DoctorStatus,
        summary: impl Into<String>,
        fix: Option<String>,
    ) -> Self {
        Self {
            id: id.to_owned(),
            status,
            summary: summary.into(),
            items: Vec::new(),
            fix,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Error)
    }
}

/// Deadlines and thresholds. Tests lower or raise them to provoke a status.
#[derive(Clone, Debug)]
pub struct DoctorOptions {
    pub comfyui_deadline: Duration,
    pub object_info_deadline: Duration,
    pub llm_deadline: Duration,
    pub workflow_deadline: Duration,
    pub disk_warning_bytes: u64,
    pub disk_error_bytes: u64,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            comfyui_deadline: Duration::from_secs(2),
            object_info_deadline: Duration::from_secs(5),
            llm_deadline: Duration::from_secs(3),
            workflow_deadline: Duration::from_secs(5),
            disk_warning_bytes: 5 * 1024 * 1024 * 1024,
            disk_error_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Runs every check against the active harness and LLM roles. `restart_required`
/// lists settings whose saved value differs from the running one.
pub async fn run(
    harness: &Harness,
    llm_roles: &LlmRoles,
    restart_required: &[String],
    options: &DoctorOptions,
) -> DoctorReport {
    let config = harness.config();
    let secret = llm_roles.config().api_key.clone();
    let secret = secret.as_deref();
    let (comfy, llm, workflows, disk) = tokio::join!(
        comfy_checks(harness, options),
        llm_checks(llm_roles, secret, options),
        discover_workflows(harness, options),
        async { disk_check(&config.runs, options) },
    );
    let ComfyChecks {
        reachable,
        version,
        object_info,
        gpu,
        protocol_note,
        node_classes,
    } = comfy;
    let [llm_reachable, llm_text, llm_vision] = llm;
    let nodes = nodes_check(&config.workflows, workflows, node_classes.as_ref());
    let ports = ports_check(config, llm_roles.config().base_url.as_str(), protocol_note);
    DoctorReport {
        checks: vec![
            DoctorCheck::ok(
                SERVER,
                format!(
                    "The GenAImage server answered on {}:{}.",
                    config.server.host, config.server.port
                ),
            ),
            config_check(config, restart_required),
            reachable,
            version,
            object_info,
            nodes,
            llm_reachable,
            llm_text,
            llm_vision,
            gpu,
            disk,
            ports,
        ],
    }
}

fn cap_items(mut items: Vec<String>) -> Vec<String> {
    if items.len() > MAX_ITEMS {
        let more = items.len() - MAX_ITEMS;
        items.truncate(MAX_ITEMS);
        items.push(format!("...and {more} more."));
    }
    items
}

/// One line, bounded, with the LLM key removed in case a provider echoed it.
fn clean(text: &str, secret: Option<&str>) -> String {
    let mut text = text.to_owned();
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "<redacted>");
    }
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > MAX_TEXT {
        let mut truncated: String = line.chars().take(MAX_TEXT).collect();
        truncated.push_str("...");
        truncated
    } else {
        line
    }
}

fn config_check(config: &ResolvedConfig, restart_required: &[String]) -> DoctorCheck {
    let mut problems = Vec::new();
    if let Err(error) = config.comfyui.validate_base_url() {
        problems.push(error.to_string());
    }
    if !config.llm.base_url.is_empty() {
        if let Err(error) = validate_endpoint(
            "llm.base_url",
            &config.llm.base_url,
            &config.llm.allowed_hosts,
        ) {
            problems.push(error.to_string());
        }
    }
    let file = config.settings_file().display();
    if !problems.is_empty() {
        return DoctorCheck::error(
            CONFIG,
            "The configuration contains invalid settings.",
            format!("Correct the listed settings in {file} or on the Settings page, then restart `genaimage serve`."),
        )
        .with_items(problems);
    }
    if !restart_required.is_empty() {
        return DoctorCheck::warning(
            CONFIG,
            "Saved settings differ from the running server.",
            format!(
                "Restart `genaimage serve` to apply: {}.",
                restart_required.join(", ")
            ),
        )
        .with_items(restart_required.to_vec());
    }
    DoctorCheck::ok(
        CONFIG,
        format!("The configuration is valid (settings file {file})."),
    )
}

struct ComfyChecks {
    reachable: DoctorCheck,
    version: DoctorCheck,
    object_info: DoctorCheck,
    gpu: DoctorCheck,
    protocol_note: Option<String>,
    node_classes: Option<BTreeSet<String>>,
}

async fn comfy_checks(harness: &Harness, options: &DoctorOptions) -> ComfyChecks {
    let url = harness.config().comfyui.base_url.clone();
    let start_fix = "Start ComfyUI and check `comfyui.base_url` on the Settings page or in genaimage.toml; restart `genaimage serve` after changing it.";
    let backend = harness.backend();
    let stats = match timeout(options.comfyui_deadline, backend.health()).await {
        Ok(Ok(health)) => health.system,
        outcome => {
            let summary = match outcome {
                Ok(Err(error)) => {
                    format!(
                        "ComfyUI at {url} did not answer: {}",
                        clean(&error.to_string(), None)
                    )
                }
                _ => format!(
                    "ComfyUI at {url} did not answer within {} s.",
                    options.comfyui_deadline.as_secs_f64()
                ),
            };
            let dependent = |id: &str| {
                DoctorCheck::skipped(
                    id,
                    "Not run: ComfyUI is not reachable.",
                    "Fix `comfyui.reachable` first.",
                )
            };
            return ComfyChecks {
                reachable: DoctorCheck::error(COMFYUI_REACHABLE, summary, start_fix),
                version: dependent(COMFYUI_VERSION),
                object_info: dependent(COMFYUI_OBJECT_INFO),
                gpu: dependent(GPU),
                protocol_note: None,
                node_classes: None,
            };
        }
    };
    let protocol_note = (!stats.get("system").is_some_and(Value::is_object)).then(|| {
        format!("{url} answered /system_stats but not in ComfyUI's format; another service may be using that port.")
    });
    let (object_info, node_classes) =
        match timeout(options.object_info_deadline, backend.object_info()).await {
            Ok(Ok(Value::Object(classes))) => (
                DoctorCheck::ok(
                    COMFYUI_OBJECT_INFO,
                    format!("ComfyUI lists {} node classes.", classes.len()),
                ),
                Some(classes.keys().cloned().collect()),
            ),
            Ok(Ok(_)) => (
                DoctorCheck::error(
                    COMFYUI_OBJECT_INFO,
                    "ComfyUI /object_info did not return a JSON object.",
                    "Check that `comfyui.base_url` points at ComfyUI itself, not a proxy or another service, and update ComfyUI if it is very old.",
                ),
                None,
            ),
            Ok(Err(error)) => (
                DoctorCheck::error(
                    COMFYUI_OBJECT_INFO,
                    format!("Fetching ComfyUI /object_info failed: {}", clean(&error.to_string(), None)),
                    "Check the ComfyUI console for errors (a custom node may fail to load), restart ComfyUI and run `genaimage doctor` again.",
                ),
                None,
            ),
            Err(_) => (
                DoctorCheck::error(
                    COMFYUI_OBJECT_INFO,
                    format!(
                        "ComfyUI /object_info did not answer within {} s.",
                        options.object_info_deadline.as_secs_f64()
                    ),
                    "ComfyUI may be busy loading models or custom nodes; wait for it to finish, then run `genaimage doctor` again.",
                ),
                None,
            ),
        };
    ComfyChecks {
        reachable: DoctorCheck::ok(COMFYUI_REACHABLE, format!("ComfyUI answered at {url}.")),
        version: version_check(&stats),
        object_info,
        gpu: gpu_check(&stats),
        protocol_note,
        node_classes,
    }
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .map(|part| part.parse::<u64>());
    Some((
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ))
}

fn version_check(stats: &Value) -> DoctorCheck {
    let fix = "Update ComfyUI to 0.3.57 or later; GenAImage relies on it to cancel a single job without interrupting others.";
    let Some(version) = stats
        .pointer("/system/comfyui_version")
        .and_then(Value::as_str)
    else {
        return DoctorCheck::warning(COMFYUI_VERSION, "ComfyUI did not report its version.", fix);
    };
    match parse_version(version) {
        Some(parsed) if parsed >= MIN_TARGETED_INTERRUPT => DoctorCheck::ok(
            COMFYUI_VERSION,
            format!("ComfyUI {}.", clean(version, None)),
        ),
        Some(_) => DoctorCheck::warning(
            COMFYUI_VERSION,
            format!("ComfyUI {} is older than 0.3.57.", clean(version, None)),
            fix,
        ),
        None => DoctorCheck::warning(
            COMFYUI_VERSION,
            format!(
                "ComfyUI reported an unrecognised version '{}'.",
                clean(version, None)
            ),
            fix,
        ),
    }
}

fn gpu_check(stats: &Value) -> DoctorCheck {
    let devices: Vec<&Value> = stats
        .get("devices")
        .and_then(Value::as_array)
        .map(|devices| devices.iter().collect())
        .unwrap_or_default();
    let name = |device: &Value| {
        clean(
            device
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed device"),
            None,
        )
    };
    let accelerators: Vec<String> = devices
        .iter()
        .filter(|device| device.get("type").and_then(Value::as_str) != Some("cpu"))
        .map(|device| name(device))
        .collect();
    if !accelerators.is_empty() {
        return DoctorCheck::ok(
            GPU,
            format!("ComfyUI sees {} GPU device(s).", accelerators.len()),
        )
        .with_items(accelerators);
    }
    let summary = if devices.is_empty() {
        "ComfyUI reported no compute devices."
    } else {
        "ComfyUI is running on the CPU only."
    };
    DoctorCheck::warning(
        GPU,
        summary,
        "Install a GPU build of PyTorch for ComfyUI (and start it without `--cpu`) so generation uses the GPU; check the ComfyUI start-up log for the device it selected.",
    )
}

struct DiscoveredGraphs {
    graphs: Result<Vec<(String, BTreeSet<String>)>, String>,
    diagnostics: Vec<String>,
}

async fn discover_workflows(harness: &Harness, options: &DoctorOptions) -> DiscoveredGraphs {
    let harness = harness.clone();
    let task = tokio::task::spawn_blocking(move || {
        let discovery = harness
            .discover_workflows()
            .map_err(|error| error.to_string())?;
        let diagnostics = discovery
            .diagnostics
            .iter()
            .map(|item| {
                format!(
                    "{}: {}",
                    item.path.display(),
                    clean(&item.error.to_string(), None)
                )
            })
            .collect();
        let graphs = discovery
            .indexable_manifest_paths
            .iter()
            .take(MAX_WORKFLOWS)
            .filter_map(|(id, path)| {
                let loaded = LoadedWorkflow::load(path).ok()?;
                let classes = loaded
                    .workflow
                    .as_object()?
                    .values()
                    .filter_map(|node| node.get("class_type").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect();
                Some((id.clone(), classes))
            })
            .collect();
        Ok::<_, String>((graphs, diagnostics))
    });
    match timeout(options.workflow_deadline, task).await {
        Ok(Ok(Ok((graphs, diagnostics)))) => DiscoveredGraphs {
            graphs: Ok(graphs),
            diagnostics,
        },
        Ok(Ok(Err(error))) => DiscoveredGraphs {
            graphs: Err(clean(&error, None)),
            diagnostics: Vec::new(),
        },
        _ => DiscoveredGraphs {
            graphs: Err("workflow discovery did not finish in time".into()),
            diagnostics: Vec::new(),
        },
    }
}

fn nodes_check(
    directory: &Path,
    discovered: DiscoveredGraphs,
    available: Option<&BTreeSet<String>>,
) -> DoctorCheck {
    let Some(available) = available else {
        return DoctorCheck::skipped(
            WORKFLOWS_NODES,
            "Not run: ComfyUI node classes are unknown.",
            "Fix `comfyui.reachable` and `comfyui.object_info` first.",
        );
    };
    let directory = directory.display();
    let graphs = match discovered.graphs {
        Ok(graphs) => graphs,
        Err(error) => {
            return DoctorCheck::error(
                WORKFLOWS_NODES,
                format!("Workflows could not be discovered: {error}"),
                format!(
                "Check that `paths.workflows` ({directory}) exists and is readable by the server."
            ),
            )
        }
    };
    let broken = discovered.diagnostics.len();
    let missing: BTreeMap<&str, Vec<&String>> = graphs
        .iter()
        .filter_map(|(id, classes)| {
            let missing: Vec<&String> = classes.difference(available).collect();
            (!missing.is_empty()).then_some((id.as_str(), missing))
        })
        .collect();
    if !missing.is_empty() {
        let items = missing
            .iter()
            .map(|(id, classes)| {
                let listed: Vec<&str> = classes
                    .iter()
                    .take(MAX_CLASSES_PER_WORKFLOW)
                    .map(|class| class.as_str())
                    .collect();
                let more = classes.len().saturating_sub(MAX_CLASSES_PER_WORKFLOW);
                let suffix = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                format!("{id}: missing {}{suffix}", listed.join(", "))
            })
            .collect();
        let summary = format!(
            "{} of {} workflows need node classes that ComfyUI does not provide.",
            missing.len(),
            graphs.len()
        );
        let fix = "Install the custom nodes that provide the missing classes in ComfyUI yourself (GenAImage never installs or substitutes nodes), restart ComfyUI and run `genaimage doctor` again; `genaimage workflow validate <ID>` rechecks one workflow.";
        let check = if missing.len() == graphs.len() {
            DoctorCheck::error(WORKFLOWS_NODES, summary, fix)
        } else {
            DoctorCheck::warning(WORKFLOWS_NODES, summary, fix)
        };
        return check.with_items(items);
    }
    if graphs.is_empty() {
        return DoctorCheck::warning(
            WORKFLOWS_NODES,
            format!("No usable workflows were found in {directory}."),
            format!("Add a *.genaimage.json manifest and its API-format workflow under {directory} (`paths.workflows`); see docs/workflow-manifest.md."),
        )
        .with_items(discovered.diagnostics);
    }
    if broken > 0 {
        return DoctorCheck::warning(
            WORKFLOWS_NODES,
            format!("{broken} workflow file(s) could not be loaded; the {} loaded workflows have every node they need.", graphs.len()),
            "Repair or move the listed manifests; `genaimage workflow list` shows the same diagnostics.",
        )
        .with_items(discovered.diagnostics);
    }
    DoctorCheck::ok(
        WORKFLOWS_NODES,
        format!(
            "All {} workflows have the node classes they need.",
            graphs.len()
        ),
    )
}

async fn llm_checks(
    roles: &LlmRoles,
    secret: Option<&str>,
    options: &DoctorOptions,
) -> [DoctorCheck; 3] {
    let config = roles.config();
    if config.base_url.is_empty() {
        let optional = "The assistant is optional. To enable it, set `llm.base_url` (and `llm.text_model`) on the Settings page or in genaimage.toml, then restart `genaimage serve`.";
        return [
            DoctorCheck::skipped(LLM_REACHABLE, "The LLM is not configured.", optional),
            DoctorCheck::skipped(LLM_TEXT, "No LLM endpoint is configured.", optional),
            DoctorCheck::skipped(
                LLM_VISION,
                "No LLM endpoint is configured.",
                "Optional: after setting `llm.base_url`, set `llm.vision_model` to enable vision review.",
            ),
        ];
    }
    let models = match timeout(options.llm_deadline, roles.refresh(true)).await {
        Ok(Ok(models)) => models,
        outcome => {
            let summary = match outcome {
                Ok(Err(error)) => format!(
                    "The LLM at {} did not return a model list: {}",
                    config.base_url,
                    clean(&error.to_string(), secret)
                ),
                _ => format!(
                    "The LLM at {} did not answer within {} s.",
                    config.base_url,
                    options.llm_deadline.as_secs_f64()
                ),
            };
            let dependent = |id: &str| {
                DoctorCheck::skipped(
                    id,
                    "Not run: the LLM is not reachable.",
                    "Fix `llm.reachable` first.",
                )
            };
            return [
                DoctorCheck::error(
                    LLM_REACHABLE,
                    summary,
                    "Start your OpenAI-compatible server and check `llm.base_url` (it must answer GET /models with {\"data\": [...]}) and, if it requires one, the write-only `llm.api_key`; use Test LLM connection on the Settings page.",
                ),
                dependent(LLM_TEXT),
                dependent(LLM_VISION),
            ];
        }
    };
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    let reachable = DoctorCheck::ok(
        LLM_REACHABLE,
        format!("The LLM answered and lists {} models.", models.len()),
    );
    [
        reachable,
        role_check(
            roles,
            LLM_TEXT,
            "llm.text_model",
            &config.text_model,
            &models,
            &ids,
        ),
        role_check(
            roles,
            LLM_VISION,
            "llm.vision_model",
            &config.vision_model,
            &models,
            &ids,
        ),
    ]
}

fn role_check(
    roles: &LlmRoles,
    id: &str,
    key: &str,
    model: &str,
    models: &[LlmModel],
    listed: &[&str],
) -> DoctorCheck {
    let is_vision = id == LLM_VISION;
    let available = || {
        let shown: Vec<&str> = listed.iter().take(5).copied().collect();
        format!("available models: {}", shown.join(", "))
    };
    if model.is_empty() {
        return if is_vision {
            DoctorCheck::skipped(
                id,
                "No vision model is configured.",
                format!(
                    "Optional: set `{key}` to a vision-capable model ({}) to enable vision review.",
                    available()
                ),
            )
        } else {
            DoctorCheck::warning(
                id,
                "No text model is configured, so the assistant cannot run.",
                format!("Set `{key}` on the Settings page or in genaimage.toml ({}) and restart `genaimage serve`.", available()),
            )
        };
    }
    let Some(found) = models.iter().find(|candidate| candidate.id == model) else {
        return DoctorCheck::error(
            id,
            format!(
                "The configured model '{}' is not served by the LLM.",
                clean(model, None)
            ),
            format!(
                "Load that model in the LLM server or set `{key}` to one it lists ({}).",
                available()
            ),
        );
    };
    let (tools, vision) = roles.capabilities(found);
    let table = format!("[llm.capabilities.\"{model}\"]");
    if is_vision && !vision {
        return DoctorCheck::error(
            id,
            format!("Model '{}' does not report image input support.", clean(model, None)),
            format!("Set `{key}` to a vision-capable model, or, if '{}' does accept images, declare it with `vision = true` under {table} in genaimage.toml.", clean(model, None)),
        );
    }
    if !is_vision && !tools {
        return DoctorCheck::warning(
            id,
            format!("Model '{}' does not report tool-calling support.", clean(model, None)),
            format!("Assistant tools will be refused. Set `{key}` to a tool-capable model, or declare `tools = true` under {table} in genaimage.toml if it does support tools."),
        );
    }
    DoctorCheck::ok(
        id,
        format!(
            "Model '{}' is available (tools: {}, vision: {}).",
            clean(model, None),
            if tools { "yes" } else { "no" },
            if vision { "yes" } else { "no" }
        ),
    )
}

fn disk_check(runs: &Path, options: &DoctorOptions) -> DoctorCheck {
    let fix = format!(
        "Free space on the volume that holds {0}, or move `paths.runs` (and `paths.state_database`) to a larger disk and restart `genaimage serve`.",
        runs.display()
    );
    let existing: Option<PathBuf> = runs
        .ancestors()
        .find(|path| path.exists())
        .map(Path::to_path_buf);
    let free = existing
        .as_deref()
        .map(fs2::available_space)
        .and_then(Result::ok);
    let Some(free) = free else {
        return DoctorCheck::warning(
            DISK,
            format!("Free space for {} could not be determined.", runs.display()),
            format!(
                "Check that the directory for `paths.runs` ({}) exists and its volume is mounted.",
                runs.display()
            ),
        );
    };
    let gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
    let summary = format!("{gib:.1} GiB free for {}.", runs.display());
    if free < options.disk_error_bytes {
        DoctorCheck::error(DISK, summary, fix)
    } else if free < options.disk_warning_bytes {
        DoctorCheck::warning(DISK, summary, fix)
    } else {
        DoctorCheck::ok(DISK, summary)
    }
}

fn endpoint_key(url: &str) -> Option<(String, u16)> {
    let url = Url::parse(url).ok()?;
    let host = url
        .host_str()?
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    let host = match host.as_str() {
        "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "::" => "loopback".to_owned(),
        _ => host,
    };
    Some((host, url.port_or_known_default()?))
}

fn ports_check(
    config: &ResolvedConfig,
    llm_url: &str,
    protocol_note: Option<String>,
) -> DoctorCheck {
    let services = [
        (
            "the GenAImage server (`server.host`/`server.port`)",
            endpoint_key(&format!(
                "http://{}:{}",
                config.server.host, config.server.port
            )),
        ),
        (
            "ComfyUI (`comfyui.base_url`)",
            endpoint_key(&config.comfyui.base_url),
        ),
        ("the LLM (`llm.base_url`)", endpoint_key(llm_url)),
    ];
    let mut conflicts = Vec::new();
    for (index, (left_name, left)) in services.iter().enumerate() {
        for (right_name, right) in &services[index + 1..] {
            if left.is_some() && left == right {
                conflicts.push(format!(
                    "{left_name} and {right_name} use the same host and port."
                ));
            }
        }
    }
    if !conflicts.is_empty() {
        return DoctorCheck::error(
            PORTS,
            "Two services are configured for the same address.",
            "Give each service its own port: change `server.port`, `comfyui.base_url` or `llm.base_url` on the Settings page or in genaimage.toml, then restart `genaimage serve`.",
        )
        .with_items(conflicts);
    }
    if let Some(note) = protocol_note {
        return DoctorCheck::warning(
            PORTS,
            "The ComfyUI address answers, but not as ComfyUI.",
            "Check which program listens on that port, stop it or correct `comfyui.base_url`, and make sure ComfyUI is running on the configured port.",
        )
        .with_items(vec![note]);
    }
    DoctorCheck::ok(
        PORTS,
        "No port conflicts among the server, ComfyUI and the LLM.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_below_targeted_interrupt_warn_with_an_upgrade_fix() {
        for stats in [
            json!({"system": {"comfyui_version": "0.3.10"}}),
            json!({"system": {"comfyui_version": "nightly"}}),
            json!({"system": {}}),
        ] {
            let check = version_check(&stats);
            assert_eq!(check.status, DoctorStatus::Warning, "{stats}");
            assert!(check.fix.unwrap().contains("0.3.57"));
        }
        let ok = version_check(&json!({"system": {"comfyui_version": "v0.4.0"}}));
        assert_eq!(ok.status, DoctorStatus::Ok);
        assert!(ok.fix.is_none());
    }

    #[test]
    fn cpu_only_and_missing_devices_warn() {
        let cpu = gpu_check(&json!({"devices": [{"name": "cpu", "type": "cpu"}]}));
        assert_eq!(cpu.status, DoctorStatus::Warning);
        assert!(cpu.summary.contains("CPU only") && cpu.fix.unwrap().contains("PyTorch"));
        assert_eq!(
            gpu_check(&json!({"devices": []})).status,
            DoctorStatus::Warning
        );
        let gpu = gpu_check(&json!({"devices": [{"name": "cuda:0 Test", "type": "cuda"}]}));
        assert_eq!(
            (gpu.status, gpu.items),
            (DoctorStatus::Ok, vec!["cuda:0 Test".to_owned()])
        );
    }

    #[test]
    fn disk_thresholds_set_the_status_and_name_the_path() {
        let directory = tempfile::tempdir().unwrap();
        let runs = directory.path().join("runs");
        let options = |warning, error| DoctorOptions {
            disk_warning_bytes: warning,
            disk_error_bytes: error,
            ..DoctorOptions::default()
        };
        assert_eq!(disk_check(&runs, &options(0, 0)).status, DoctorStatus::Ok);
        let warning = disk_check(&runs, &options(u64::MAX, 0));
        assert_eq!(warning.status, DoctorStatus::Warning);
        let error = disk_check(&runs, &options(u64::MAX, u64::MAX));
        assert_eq!(error.status, DoctorStatus::Error);
        assert!(error.fix.unwrap().contains(&runs.display().to_string()));
    }

    #[test]
    fn endpoints_compare_by_normalised_host_and_port() {
        assert_eq!(
            endpoint_key("http://localhost:8188"),
            endpoint_key("http://127.0.0.1:8188/")
        );
        assert_ne!(
            endpoint_key("http://127.0.0.1:8188"),
            endpoint_key("http://127.0.0.1:8189")
        );
        assert_eq!(
            endpoint_key("http://[::1]:80"),
            endpoint_key("http://localhost")
        );
    }

    #[test]
    fn provider_text_is_bounded_and_loses_the_key() {
        let echoed = format!("bad key sk-secret-123\n{}", "x".repeat(1000));
        let cleaned = clean(&echoed, Some("sk-secret-123"));
        assert!(!cleaned.contains("sk-secret-123") && cleaned.contains("<redacted>"));
        assert!(cleaned.chars().count() <= MAX_TEXT + 3 && !cleaned.contains('\n'));
    }

    #[test]
    fn long_item_lists_are_capped_with_a_count() {
        let items = (0..50).map(|n| n.to_string()).collect();
        let check = DoctorCheck::ok("x", "y").with_items(items);
        assert_eq!(check.items.len(), MAX_ITEMS + 1);
        assert_eq!(check.items.last().unwrap(), "...and 30 more.");
    }

    fn config_with(toml: &str) -> (tempfile::TempDir, ResolvedConfig) {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("genaimage.toml");
        std::fs::write(&file, toml).unwrap();
        let root = directory.path().display().to_string();
        let config = crate::HarnessConfig::load_with(
            directory.path(),
            Some(&file),
            directory.path(),
            |key| matches!(key, "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME").then(|| root.clone()),
        )
        .unwrap();
        (directory, config)
    }

    #[test]
    fn services_sharing_an_address_are_a_port_error_naming_the_settings() {
        let (_dir, config) =
            config_with("[server]\nport = 8188\n[comfyui]\nbase_url = \"http://localhost:8188\"\n");
        let check = ports_check(&config, "", None);
        assert_eq!(check.status, DoctorStatus::Error);
        assert!(check.items[0].contains("server") && check.items[0].contains("ComfyUI"));
        assert!(check.fix.unwrap().contains("server.port"));

        let (_dir, config) =
            config_with("[server]\nport = 3000\n[comfyui]\nbase_url = \"http://127.0.0.1:8188\"\n");
        let clash = ports_check(&config, "http://127.0.0.1:8188/v1", None);
        assert_eq!(clash.status, DoctorStatus::Error);
        assert!(clash.items[0].contains("ComfyUI") && clash.items[0].contains("LLM"));
        assert_eq!(
            ports_check(&config, "http://127.0.0.1:1234/v1", None).status,
            DoctorStatus::Ok
        );
    }

    #[test]
    fn a_comfyui_port_that_speaks_another_protocol_is_a_warning() {
        let (_dir, config) = config_with("");
        let check = ports_check(&config, "", Some("answered in another format".into()));
        assert_eq!(check.status, DoctorStatus::Warning);
        assert!(check.fix.unwrap().contains("comfyui.base_url"));
    }

    #[test]
    fn invalid_endpoint_settings_are_a_config_error() {
        let (_dir, mut config) = config_with("");
        config.llm.base_url = "http://example.com/v1".into();
        let check = config_check(&config, &[]);
        assert_eq!(check.status, DoctorStatus::Error);
        assert!(check.items[0].contains("llm.base_url"));
        assert!(check.fix.unwrap().contains("Settings page"));
    }
}
