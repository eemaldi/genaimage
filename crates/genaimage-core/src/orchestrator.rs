use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    artifacts::{self, InputFileRecord},
    assistant::{ConversationService, ConversationStore, PromptTemplates},
    config::{ResolvedConfig, SettingsUpdate},
    event::RunLockKind,
    image_backend::{BackendHealth, HistoryOutcome, ImageBackend, PromptResponse, UploadResponse},
    media,
    persistence::{
        database_matches_settings, BatchLink, NewRun, OutputRecord, RunEvent, RunRecord,
        RunRepository, RunStatus, SubmissionReservation,
    },
    uploads::{detected_type, UploadMetadata, UploadStore},
    workflow::{
        read_regular_file, workflow_revision, BoundWorkflow, GenerationMode, LoadedWorkflow,
        ParameterError, ParameterKind, WorkflowManifest,
    },
    Event, EventBus, HarnessError, LlmProvider, Result,
};

mod batches;
pub use batches::{BatchCreation, BatchDriver, RunObserver};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunRequest {
    pub workflow: String,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub version: Option<String>,
    // Omitting the field preserves idempotency fingerprints for unpinned requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileResult {
    pub run: RunRecord,
    pub outputs: Vec<OutputRecord>,
    pub outcome: ReconcileOutcome,
    pub settles_at: Option<DateTime<Utc>>,
}

/// What this check established, independently of the durable run status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Terminal,
    Active,
    InProgressElsewhere,
    /// The prompt was absent from queue and history after the settle window.
    /// ComfyUI cannot prove absence; very late admission is still possible.
    NotFound,
    StillAmbiguous,
}

const ADMISSION_GRACE_SECONDS: u64 = 10;

/// Result of a durable submission and whether its key was already reserved.
pub struct KeyedSubmission {
    pub run: RunRecord,
    pub replayed: bool,
}

enum SubmissionRequest {
    New(RunRequest),
    Rerun(String),
}

#[derive(Deserialize)]
struct RecordedInputs {
    preset: Option<String>,
    parameters: Value,
    source_inputs: Vec<InputFileRecord>,
}

struct VerifiedInput {
    record: InputFileRecord,
    snapshot: InputSnapshot,
}

struct InputSnapshot {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

const WORKFLOW_MISMATCH: HarnessError =
    HarnessError::RerunMismatch("The workflow graph or bindings no longer match the source run.");
const INPUT_MISMATCH: HarnessError =
    HarnessError::RerunMismatch("An input image is missing or no longer matches the source run.");
const MISSING_PROVENANCE: HarnessError = HarnessError::RerunMismatch(
    "The workflow bindings or recorded inputs cannot be verified for the source run.",
);

struct TransitionGuard {
    bus: EventBus,
    published: bool,
}

impl TransitionGuard {
    fn new(bus: &EventBus) -> Self {
        Self {
            bus: bus.clone(),
            published: false,
        }
    }

    fn disarm(mut self) {
        self.published = true;
    }

    fn publish(mut self, event: Event) {
        self.bus.publish(event);
        self.published = true;
    }
}

impl Drop for TransitionGuard {
    fn drop(&mut self) {
        if !self.published {
            self.bus.invalidate();
        }
    }
}

pub(crate) async fn before_deadline<T>(
    deadline: &CancellationToken,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = deadline.cancelled() => Err(HarnessError::ShuttingDown),
        result = operation => result,
    }
}

fn transition_task_error(error: tokio::task::JoinError) -> HarnessError {
    HarnessError::Database(format!("run transition task failed: {error}"))
}

#[derive(Debug, Default)]
pub struct WorkflowDiscovery {
    pub workflows: Vec<DiscoveredWorkflow>,
    /// Paths of unique, valid IDs safe to resolve from this discovery snapshot.
    pub indexable_manifest_paths: BTreeMap<String, PathBuf>,
    /// IDs blocked by duplicates or invalid manifests in this snapshot.
    pub unresolvable_ids: BTreeSet<String>,
    /// API workflow files referenced by valid and invalid sidecars.
    pub referenced_workflow_paths: BTreeSet<PathBuf>,
    /// Safe relative thumbnail references declared by discovered manifests.
    pub referenced_thumbnail_paths: BTreeSet<PathBuf>,
    /// Failures that prevent an authoritative uniqueness check.
    pub traversal_failures: Vec<WorkflowTraversalFailure>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
    /// For each ID claimed by several valid manifests, a message naming every claimant.
    pub ambiguity: BTreeMap<String, String>,
    /// The packs scanned for this snapshot.
    pub packs: crate::pack::PackDiscovery,
}

/// Where a workflow was discovered: the writable user root or a read-only pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowSource {
    User,
    Pack(String),
}

impl std::fmt::Display for WorkflowSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => formatter.write_str("user"),
            Self::Pack(id) => write!(formatter, "pack:{id}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveredWorkflow {
    pub manifest: WorkflowManifest,
    pub revision: String,
    pub source: WorkflowSource,
    pub manifest_path: PathBuf,
    /// The pack directory a pack workflow, its graph and its thumbnail must stay inside.
    pub boundary: Option<PathBuf>,
    /// Set when another manifest claims the same ID, so none can be resolved.
    pub unavailable_reason: Option<String>,
}

impl WorkflowDiscovery {
    /// Everything wrong with one pack's declared files and workflows. A pack
    /// with any problem must never be reported as fully valid.
    pub fn pack_problems(&self, pack: &crate::pack::LoadedPack) -> Vec<String> {
        let id = &pack.manifest.id;
        let mut problems: Vec<String> = pack
            .diagnostics
            .iter()
            .map(|item| item.message.clone())
            .collect();
        for path in pack
            .workflow_paths
            .iter()
            .filter(|path| !self.workflows.iter().any(|w| w.manifest_path == **path))
        {
            problems.extend(
                self.diagnostics
                    .iter()
                    .filter(|item| item.path == *path)
                    .map(|item| format!("{}: {}", path.display(), item.error)),
            );
        }
        problems.extend(self.workflows.iter().filter_map(|workflow| {
            let reason = workflow.unavailable_reason.as_ref()?;
            matches!(&workflow.source, WorkflowSource::Pack(source) if source == id)
                .then(|| format!("{}: {reason}", workflow.manifest_path.display()))
        }));
        problems.extend(crate::templates::pack_template_problems(pack));
        problems
    }
}

fn describe_claim(path: &Path, source: &WorkflowSource) -> String {
    format!("{source} '{}'", path.display())
}

fn ambiguous_error(
    id: &str,
    claims: impl Iterator<Item = (PathBuf, WorkflowSource)>,
    hint: &str,
) -> HarnessError {
    let claimants: Vec<String> = claims
        .map(|(path, source)| describe_claim(&path, &source))
        .collect();
    HarnessError::WorkflowAmbiguous(format!(
        "workflow id '{id}' is ambiguous: declared by {}; {hint}",
        claimants.join(", ")
    ))
}

#[derive(Clone, Debug)]
pub struct WorkflowTraversalFailure {
    pub path: PathBuf,
    pub reason: String,
}

impl WorkflowTraversalFailure {
    pub fn for_id(&self, id: &str) -> HarnessError {
        HarnessError::WorkflowDiscoveryIncomplete {
            id: id.to_owned(),
            path: self.path.clone(),
            reason: self.reason.clone(),
        }
    }
}

#[derive(Debug)]
pub struct WorkflowDiagnostic {
    pub path: PathBuf,
    pub error: HarnessError,
}

#[derive(Clone)]
pub struct Harness {
    config: ResolvedConfig,
    repository: RunRepository,
    backend: Arc<dyn ImageBackend>,
    events: EventBus,
    last_health: Arc<Mutex<Option<Value>>>,
    database_identity: PathBuf,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Harness")
            .field("config", &self.config)
            .field("database_identity", &self.database_identity)
            .finish_non_exhaustive()
    }
}

impl Harness {
    pub fn new(config: ResolvedConfig, backend: Arc<dyn ImageBackend>) -> Result<Self> {
        Self::with_event_bus(config, backend, EventBus::default())
    }

    pub fn with_event_bus(
        config: ResolvedConfig,
        backend: Arc<dyn ImageBackend>,
        events: EventBus,
    ) -> Result<Self> {
        let repository = RunRepository::open(&config.state_database)?.with_identity(
            &config.workspace,
            backend.name(),
            backend.base_url(),
            config.claim_legacy_state,
        )?;
        let database_identity = fs::canonicalize(&config.state_database)
            .unwrap_or_else(|_| config.state_database.clone());
        Ok(Self {
            config,
            repository,
            backend,
            events,
            last_health: Arc::new(Mutex::new(None)),
            database_identity,
        })
    }

    /// Opens the state database on the blocking pool, using the supplied backend.
    pub async fn new_async(config: ResolvedConfig, backend: Arc<dyn ImageBackend>) -> Result<Self> {
        Self::with_event_bus_async(config, backend, EventBus::default()).await
    }

    pub async fn with_event_bus_async(
        config: ResolvedConfig,
        backend: Arc<dyn ImageBackend>,
        events: EventBus,
    ) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::with_event_bus(config, backend, events))
            .await
            .map_err(|error| {
                HarnessError::Database(format!("state database task failed: {error}"))
            })?
    }

    fn backend_error(&self, message: String) -> HarnessError {
        HarnessError::Backend {
            backend: self.backend.name().to_owned(),
            message,
        }
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub(crate) fn backend(&self) -> &Arc<dyn ImageBackend> {
        &self.backend
    }

    /// Persists an update and prepares the live snapshot. Call on the blocking
    /// pool and serialise publishers. Listener, storage and endpoint identity
    /// are fixed until restart, when database ownership is checked again.
    pub fn apply_settings(&self, update: SettingsUpdate) -> Result<(ResolvedConfig, Self)> {
        if update.validate_database_path()? {
            let proposed = update.config();
            let endpoint = proposed.comfyui.endpoint_url()?;
            let path = &proposed.state_database;
            let compatible = database_matches_settings(
                path,
                proposed.workspace.to_str().expect("validated workspace"),
                self.backend.name(),
                endpoint.as_str(),
            )
            .map_err(|_| HarnessError::InvalidSetting {
                key: "paths.state_database".into(),
                value: path.display().to_string(),
                reason:
                    "use an empty SQLite database, a supported GenAImage database or a new file"
                        .into(),
            })?;
            if !compatible {
                return Err(HarnessError::SettingsDatabaseConflict {
                    key: if endpoint.as_str() != self.backend.base_url() { "comfyui.base_url" } else { "paths.state_database" },
                    reason: "The selected state database is owned by a different workspace or endpoint, or contains unclaimed legacy runs.",
                });
            }
        }
        let saved = update.persist()?;
        let mut live = self.clone();
        live.config.llm = saved.llm.clone();
        let timing = saved.comfyui.request_timeout_seconds;
        let reconnect = saved.comfyui.websocket_reconnect_seconds;
        if timing != self.config.comfyui.request_timeout_seconds
            || reconnect != self.config.comfyui.websocket_reconnect_seconds
        {
            if let Some(backend) = self.backend.with_timing(timing, reconnect) {
                live.backend = backend;
                live.config.comfyui.request_timeout_seconds = timing;
                live.config.comfyui.websocket_reconnect_seconds = reconnect;
            }
        }
        Ok((saved, live))
    }

    pub fn events(&self) -> &EventBus {
        &self.events
    }

    pub async fn health(&self) -> Result<BackendHealth> {
        let result = self.backend.health().await;
        let (health, error) = match &result {
            Ok(health) => (Some(health.clone()), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let current = json!({"health": health, "error": error});
        let mut previous = self.last_health.lock().expect("health mutex poisoned");
        if previous.as_ref() != Some(&current) {
            *previous = Some(current);
            self.events.publish(Event::BackendHealthChanged {
                backend: self.backend.name().to_owned(),
                health,
                error,
            });
        }
        result
    }

    pub fn repository(&self) -> &RunRepository {
        &self.repository
    }

    pub fn conversation_store(&self) -> ConversationStore {
        self.repository.conversation_store()
    }

    pub fn conversation_service(
        &self,
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
    ) -> Result<ConversationService> {
        ConversationService::new(
            self.conversation_store(),
            PromptTemplates::new(self.config.data_dir()),
            provider,
            model,
        )
    }

    /// Visits the user root, then every pack's declared manifests. A pack
    /// workflow whose manifest or API graph resolves outside its pack is an
    /// invalid manifest, rejected before it is read.
    fn visit_all_workflows(
        &self,
        mut visitor: impl FnMut(&Path, &WorkflowSource, WorkflowVisit),
    ) -> crate::pack::PackDiscovery {
        visit_workflows(&self.config.workflows, &self.config.packs, |path, visit| {
            visitor(path, &WorkflowSource::User, visit)
        });
        let packs = self.discover_packs();
        for pack in &packs.packs {
            let source = WorkflowSource::Pack(pack.manifest.id.clone());
            for path in &pack.workflow_paths {
                let visit = match pack.load_workflow(path) {
                    Err(HarnessError::Io(error)) => WorkflowVisit::Incomplete(error),
                    result => WorkflowVisit::Manifest(result.map(Box::new)),
                };
                visitor(path, &source, visit);
            }
        }
        packs
    }

    pub fn discover_workflows(&self) -> Result<WorkflowDiscovery> {
        let mut discovery = WorkflowDiscovery::default();
        let mut ambiguous = BTreeSet::new();
        let mut claims: BTreeMap<String, Vec<(PathBuf, WorkflowSource)>> = BTreeMap::new();
        let mut invalid_claims: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let packs = self.visit_all_workflows(|path, source, workflow| match workflow {
            WorkflowVisit::Incomplete(error) => {
                discovery.traversal_failures.push(WorkflowTraversalFailure {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                });
                discovery.diagnostics.push(WorkflowDiagnostic {
                    path: path.to_path_buf(),
                    error: error.into(),
                });
            }
            WorkflowVisit::Diagnostic(error) => {
                discovery.diagnostics.push(WorkflowDiagnostic {
                    path: path.to_path_buf(),
                    error: error.into(),
                });
            }
            WorkflowVisit::Manifest(Ok(loaded))
                if crate::workflow::is_reserved_workflow_id(&loaded.manifest.id) =>
            {
                discovery.diagnostics.push(WorkflowDiagnostic {
                    path: path.to_path_buf(),
                    error: HarnessError::Workflow(format!(
                        "workflow id '{}' is a reserved URL path segment; choose a different ID",
                        loaded.manifest.id
                    )),
                });
            }
            WorkflowVisit::Manifest(Ok(loaded)) => {
                if let Some(thumbnail) = loaded.manifest.thumbnail.as_deref() {
                    let relative = Path::new(thumbnail);
                    if !relative.as_os_str().is_empty()
                        && relative.components().all(|component| {
                            matches!(
                                component,
                                std::path::Component::Normal(_) | std::path::Component::CurDir
                            )
                        })
                    {
                        if let Some(directory) = path.parent() {
                            discovery
                                .referenced_thumbnail_paths
                                .insert(directory.join(relative));
                        }
                    }
                }
                discovery
                    .referenced_workflow_paths
                    .insert(loaded.workflow_path.clone());
                let id = loaded.manifest.id.clone();
                claims
                    .entry(id.clone())
                    .or_default()
                    .push((path.to_path_buf(), source.clone()));
                if discovery
                    .indexable_manifest_paths
                    .insert(id.clone(), path.to_path_buf())
                    .is_some()
                {
                    ambiguous.insert(id);
                }
                discovery.workflows.push(DiscoveredWorkflow {
                    revision: loaded.revision(),
                    manifest: loaded.manifest,
                    source: source.clone(),
                    manifest_path: path.to_path_buf(),
                    boundary: None,
                    unavailable_reason: None,
                });
            }
            WorkflowVisit::Manifest(Err(error)) => {
                if let Some(source) = sidecar_source(path) {
                    if let Some(id) = source.get("id").and_then(Value::as_str) {
                        ambiguous.insert(id.to_owned());
                        invalid_claims
                            .entry(id.to_owned())
                            .or_default()
                            .push(path.to_path_buf());
                    }
                    if let Some(workflow) = source.get("workflow").and_then(Value::as_str) {
                        discovery.referenced_workflow_paths.insert(
                            path.parent()
                                .unwrap_or_else(|| Path::new("."))
                                .join(workflow),
                        );
                    }
                }
                discovery.diagnostics.push(WorkflowDiagnostic {
                    path: path.to_path_buf(),
                    error,
                });
            }
        });
        for id in &ambiguous {
            discovery.indexable_manifest_paths.remove(id);
            let everyone: Vec<(PathBuf, String)> = claims
                .get(id)
                .into_iter()
                .flatten()
                .map(|(path, source)| (path.clone(), describe_claim(path, source)))
                .chain(invalid_claims.get(id).into_iter().flatten().map(|path| {
                    (
                        path.clone(),
                        format!("invalid manifest '{}'", path.display()),
                    )
                }))
                .collect();
            if everyone.len() > 1 {
                let all = everyone
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if claims.get(id).is_some_and(|valid| valid.len() > 1) {
                    discovery.ambiguity.insert(
                        id.clone(),
                        format!("workflow id '{id}' is ambiguous: declared by {all}; remove or rename all but one"),
                    );
                }
                for workflow in discovery
                    .workflows
                    .iter_mut()
                    .filter(|w| w.manifest.id == *id)
                {
                    let others = everyone
                        .iter()
                        .filter(|(path, _)| *path != workflow.manifest_path)
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    workflow.unavailable_reason = Some(format!(
                        "workflow id '{id}' is also declared by {others}; none of them can be used until the duplicates are removed or renamed"
                    ));
                }
            }
            let Some(claimants) = claims.get(id).filter(|claimants| claimants.len() > 1) else {
                continue;
            };
            let described = claimants
                .iter()
                .map(|(path, source)| format!("{source} '{}'", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            discovery.diagnostics.push(WorkflowDiagnostic {
                path: claimants[0].0.clone(),
                error: HarnessError::Workflow(format!(
                    "workflow id '{id}' is declared more than once ({described}); the id is unavailable until the duplicates are removed"
                )),
            });
        }
        discovery
            .diagnostics
            .extend(
                packs
                    .all_diagnostics()
                    .into_iter()
                    .map(|item| WorkflowDiagnostic {
                        path: item.path,
                        error: HarnessError::Pack(item.message),
                    }),
            );
        for workflow in &mut discovery.workflows {
            if let WorkflowSource::Pack(id) = &workflow.source {
                workflow.boundary = packs.pack(id).map(|pack| pack.canonical_dir.clone());
            }
        }
        discovery.packs = packs;
        discovery.unresolvable_ids = ambiguous;
        discovery
            .traversal_failures
            .sort_by(|left, right| left.path.cmp(&right.path));
        discovery
            .workflows
            .sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        discovery
            .diagnostics
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(discovery)
    }

    pub fn load_workflow(&self, workflow: &str) -> Result<LoadedWorkflow> {
        let direct = Path::new(workflow);
        if direct.is_absolute() {
            return LoadedWorkflow::load(direct);
        }
        for candidate in [
            self.config.workspace.join(direct),
            self.config.workflows.join(direct),
        ] {
            if candidate.is_file() {
                return LoadedWorkflow::load(&candidate);
            }
        }

        let mut matches = Vec::new();
        let mut invalid_matches = Vec::new();
        let mut skipped = false;
        let mut incomplete = Vec::new();
        self.visit_all_workflows(|path, source, result| match result {
            WorkflowVisit::Incomplete(error) => incomplete.push(WorkflowTraversalFailure {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }),
            WorkflowVisit::Manifest(Ok(loaded)) if loaded.manifest.id == workflow => {
                matches.push((*loaded, source.clone()))
            }
            WorkflowVisit::Manifest(Err(error)) => {
                skipped = true;
                if sidecar_declares_id(path, workflow) {
                    invalid_matches.push((path.to_path_buf(), error));
                }
            }
            _ => {}
        });
        if let Some(failure) = incomplete.iter().min_by_key(|failure| &failure.path) {
            return Err(failure.for_id(workflow));
        }
        if let Some((path, error)) = invalid_matches.iter().min_by_key(|(path, _)| path) {
            return Err(HarnessError::Workflow(format!(
                "cannot resolve workflow id '{workflow}': invalid manifest '{}' declares it ({error}); select an explicit manifest path",
                path.display()
            )));
        }
        if matches.is_empty() {
            for candidate in [
                self.config
                    .workflows
                    .join(format!("{workflow}.genaimage.json")),
                self.config
                    .workflows
                    .join(workflow)
                    .join("workflow.genaimage.json"),
            ] {
                if candidate.is_file() {
                    return LoadedWorkflow::load(&candidate);
                }
            }
        }
        match matches.len() {
            0 => Err(HarnessError::Workflow(format!(
                "workflow '{workflow}' was not found below '{}' or in any workflow pack{}",
                self.config.workflows.display(),
                if skipped {
                    "; some manifests could not be loaded; use 'workflow list' for details"
                } else {
                    ""
                }
            ))),
            1 => Ok(matches.remove(0).0),
            _ => Err(ambiguous_error(
                workflow,
                matches
                    .into_iter()
                    .map(|(loaded, source)| (loaded.manifest_path, source)),
                "select an explicit manifest path",
            )),
        }
    }

    /// Resolves an identifier only, without interpreting it as a filesystem path.
    pub fn load_workflow_by_id(&self, id: &str) -> Result<Option<LoadedWorkflow>> {
        let mut matches = Vec::new();
        let mut invalid_matches = Vec::new();
        let mut incomplete = Vec::new();
        self.visit_all_workflows(|path, source, result| match result {
            WorkflowVisit::Incomplete(error) => incomplete.push(WorkflowTraversalFailure {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }),
            WorkflowVisit::Manifest(Ok(loaded)) if loaded.manifest.id == id => {
                matches.push((*loaded, source.clone()))
            }
            WorkflowVisit::Manifest(Err(error)) if sidecar_declares_id(path, id) => {
                invalid_matches.push((path.to_path_buf(), error));
            }
            _ => {}
        });
        if let Some(failure) = incomplete.iter().min_by_key(|failure| &failure.path) {
            return Err(failure.for_id(id));
        }
        if let Some((path, error)) = invalid_matches.iter().min_by_key(|(path, _)| path) {
            return Err(HarnessError::Workflow(format!(
                "cannot resolve workflow id '{id}': invalid manifest '{}' declares it ({error})",
                path.display()
            )));
        }
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.pop().map(|(loaded, _)| loaded)),
            _ => Err(ambiguous_error(
                id,
                matches
                    .into_iter()
                    .map(|(loaded, source)| (loaded.manifest_path, source)),
                "remove or rename all but one",
            )),
        }
    }

    /// Resolve and optionally pin a workflow by ID before using the returned snapshot.
    pub fn load_api_workflow(
        &self,
        id: &str,
        revision: Option<&str>,
    ) -> Result<Option<LoadedWorkflow>> {
        match self.load_workflow_by_id(id) {
            Ok(Some(loaded)) => {
                loaded.verify_revision(revision)?;
                Ok(Some(loaded))
            }
            Ok(None) | Err(HarnessError::Workflow(_)) if revision.is_some() => {
                Err(HarnessError::WorkflowChanged)
            }
            result => result,
        }
    }

    /// Returns required node classes absent from the backend, or an error if
    /// the check could not be completed.
    pub async fn missing_workflow_nodes(&self, workflow: &Value) -> Result<Vec<String>> {
        let nodes = workflow.as_object().ok_or_else(|| {
            HarnessError::Workflow("API workflow root must be an object".to_owned())
        })?;
        let class_types: BTreeSet<String> = nodes
            .values()
            .filter_map(|node| node.get("class_type").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let missing_class_types: Vec<&str> = nodes
            .iter()
            .filter_map(|(node_id, node)| {
                node.get("class_type")
                    .and_then(Value::as_str)
                    .is_none()
                    .then_some(node_id.as_str())
            })
            .collect();
        if !missing_class_types.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "nodes {} are missing class_type",
                missing_class_types.join(", ")
            )));
        }
        let available = self.backend.object_info().await?;
        let available = available.as_object().ok_or_else(|| {
            self.backend_error(format!(
                "{} /object_info did not return an object; cannot validate nodes",
                self.backend.name()
            ))
        })?;
        Ok(class_types
            .into_iter()
            .filter(|class_type| !available.contains_key(class_type))
            .collect())
    }

    async fn validate_workflow_nodes(&self, workflow: &Value) -> Result<()> {
        let unavailable = self.missing_workflow_nodes(workflow).await?;
        if !unavailable.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "{} does not provide required node types: {}",
                self.backend.name(),
                unavailable.join(", ")
            )));
        }
        Ok(())
    }

    pub async fn submit(&self, request: RunRequest) -> Result<RunRecord> {
        Ok(self
            .submit_inner(
                SubmissionRequest::New(request),
                None,
                None,
                &CancellationToken::new(),
            )
            .await?
            .run)
    }

    /// Submits a configured workflow by ID with a key scoped to the state database.
    /// Dropping the caller does not cancel the attempt.
    pub async fn submit_keyed(&self, key: String, request: RunRequest) -> Result<KeyedSubmission> {
        let harness = self.clone();
        tokio::spawn(async move {
            harness
                .submit_keyed_until(key, request, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Like `submit_keyed`, but the caller owns the task and can end network
    /// acknowledgement waits via `deadline`. Await it through durable finalisation.
    pub async fn submit_keyed_until(
        &self,
        key: String,
        request: RunRequest,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        self.submit_keyed_request(key, SubmissionRequest::New(request), None, deadline)
            .await
    }

    /// Submit a distinct run from recorded provenance, independent of source status.
    /// Dropping the caller does not cancel the attempt.
    pub async fn rerun(&self, key: String, id: String) -> Result<KeyedSubmission> {
        let harness = self.clone();
        tokio::spawn(async move {
            harness
                .rerun_until(key, id, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// As with `submit_keyed_until`, await this through durable finalisation.
    pub async fn rerun_until(
        &self,
        key: String,
        id: String,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        self.submit_keyed_request(key, SubmissionRequest::Rerun(id), None, deadline)
            .await
    }

    async fn submit_keyed_request(
        &self,
        key: String,
        request: SubmissionRequest,
        batch: Option<BatchLink>,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        let fingerprint = match &request {
            // Keep existing fingerprints stable. The tuple cannot serialise as a submit body.
            SubmissionRequest::New(request) => serde_json::to_vec(request)?,
            SubmissionRequest::Rerun(id) => serde_json::to_vec(&("rerun", id))?,
        };
        let fingerprint = hex::encode(Sha256::digest(fingerprint));
        let lock = self
            .events
            .run_lock(&self.database_identity, &key, RunLockKind::SubmitKey);
        let _key_guard = tokio::select! {
            biased;
            _ = deadline.cancelled() => return Err(HarnessError::ShuttingDown),
            guard = lock.lock() => guard,
        };
        let repository = self.repository.as_async();
        if let Some(run) = repository
            .find_submission(key.clone(), fingerprint.clone())
            .await?
        {
            return Ok(KeyedSubmission {
                run: self.recover_submission(run).await?.0,
                replayed: true,
            });
        }
        self.submit_inner(request, Some((key, fingerprint)), batch, deadline)
            .await
    }

    // Acquire before publishing the run in SQLite. The OS releases ownership
    // on process death, even if a different Harness is serving this database.
    fn try_claim_submission(&self, run_id: &str) -> Result<Option<File>> {
        let mut directory = self.database_identity.as_os_str().to_os_string();
        directory.push(".submissions");
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        let name = hex::encode(Sha256::digest(run_id.as_bytes()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(name))?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(file)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Return the refreshed run and whether it is still owned by a live task.
    async fn recover_submission(&self, run: RunRecord) -> Result<(RunRecord, bool)> {
        if run.status != RunStatus::Submitting
            || !self
                .repository
                .as_async()
                .uses_submission_lock(&run.id)
                .await?
        {
            return Ok((run, false));
        }
        let Some(_owner) = self.try_claim_submission(&run.id)? else {
            return Ok((run, true));
        };
        let run = self
            .mark_submission_unknown(
                &run.id,
                "The submission owner exited before the prompt acknowledgement was recorded.",
            )
            .await?;
        Ok((run, false))
    }

    pub fn uploads(&self) -> UploadStore {
        UploadStore::new(&self.config.uploads, self.config.server.max_upload_bytes)
    }

    /// Validate a proposal, including upload IDs when image parameters are present.
    pub async fn validate_proposal_with_uploads(
        &self,
        workflow: &LoadedWorkflow,
        preset: Option<&str>,
        parameters: &BTreeMap<String, Value>,
    ) -> std::result::Result<
        crate::assistant::suggest::ValidatedProposal,
        Vec<crate::assistant::suggest::ProposalIssue>,
    > {
        let proposal = crate::assistant::suggest::validate_proposal_with_options(
            workflow,
            preset,
            parameters,
            crate::assistant::suggest::ImageUploadPolicy::Allow,
        )?;
        let errors = self
            .validate_api_bindings(workflow, preset, parameters)
            .await
            .map_err(|error| {
                vec![crate::assistant::suggest::ProposalIssue {
                    field: "parameters".into(),
                    reason: error.to_string(),
                }]
            })?;
        if errors.is_empty() {
            Ok(proposal)
        } else {
            Err(errors
                .into_iter()
                .map(|error| crate::assistant::suggest::ProposalIssue {
                    field: format!("parameters.{}", error.parameter),
                    reason: error.reason,
                })
                .collect())
        }
    }

    /// Validate server bindings without uploading anything to the backend.
    pub async fn validate_api_bindings(
        &self,
        workflow: &LoadedWorkflow,
        preset: Option<&str>,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<Vec<ParameterError>> {
        Ok(self
            .check_api_bindings(workflow, preset, parameters)
            .await?
            .0)
    }

    async fn check_api_bindings(
        &self,
        workflow: &LoadedWorkflow,
        preset: Option<&str>,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<(Vec<ParameterError>, BTreeMap<String, UploadMetadata>)> {
        let mut errors = workflow.validate_bindings(preset, parameters)?;
        let preset = workflow.preset_values(preset)?;
        let store = self.uploads();
        let mut uploads = BTreeMap::new();
        for (name, definition) in &workflow.manifest.parameters {
            if definition.kind != ParameterKind::ImageUpload
                || errors.iter().any(|error| error.parameter == *name)
            {
                continue;
            }
            let Some(id) = parameters
                .get(name)
                .or_else(|| preset.get(name))
                .or(definition.default.as_ref())
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Some(metadata) = store.metadata(id).await? {
                uploads.insert(name.clone(), metadata);
            } else {
                errors.push(ParameterError {
                    parameter: name.clone(),
                    reason: "Unknown upload ID. Use an ID returned by the uploads API, not a local path.".into(),
                });
            }
        }
        Ok((errors, uploads))
    }

    fn rerun_provenance(&self, run: &RunRecord) -> Result<(String, RecordedInputs)> {
        let hash = run
            .workflow_manifest_sha256
            .clone()
            .ok_or(HarnessError::RerunMissingManifest)?;
        let expected = run.preflight_sha256.as_ref().ok_or(MISSING_PROVENANCE)?;
        let path = Path::new(&run.output_dir).join("preflight.json");
        let mut file =
            media::open_stored(&self.config.runs, &path).map_err(|_| MISSING_PROVENANCE)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| MISSING_PROVENANCE)?;
        if hex::encode(Sha256::digest(&bytes)) != *expected {
            return Err(MISSING_PROVENANCE);
        }
        let inputs = serde_json::from_slice(&bytes).map_err(|_| MISSING_PROVENANCE)?;
        Ok((hash, inputs))
    }

    async fn verified_rerun_inputs(
        &self,
        bound: &BoundWorkflow,
        inputs: Vec<InputFileRecord>,
    ) -> Result<Vec<VerifiedInput>> {
        let count = inputs.len();
        let mut inputs: BTreeMap<_, _> = inputs
            .into_iter()
            .map(|record| (record.parameter.clone(), record))
            .collect();
        if count != inputs.len() || count != bound.image_uploads.len() {
            return Err(INPUT_MISMATCH);
        }
        let store = self.uploads();
        let mut verified = Vec::new();
        for (parameter, path) in &bound.image_uploads {
            let mut record = inputs.remove(parameter).ok_or(INPUT_MISMATCH)?;
            record.uploaded_name.clear();
            let (source, filename, media_type) = if let Some(id) = &record.upload_id {
                if path.to_str() != Some(id.as_str()) {
                    return Err(INPUT_MISMATCH);
                }
                let metadata = store
                    .metadata(id)
                    .await
                    .map_err(|_| INPUT_MISMATCH)?
                    .ok_or(INPUT_MISMATCH)?;
                if metadata.sha256 != record.sha256 || metadata.byte_size != record.byte_size {
                    return Err(INPUT_MISMATCH);
                }
                let file = store
                    .open_image(&metadata)
                    .await
                    .map_err(|_| INPUT_MISMATCH)?
                    .into_std()
                    .await;
                let filename = format!("genaimage-{}.{}", Uuid::new_v4(), metadata.extension());
                (file, filename, Some(metadata.media_type))
            } else {
                let path = resolve_input_path(&self.config.workspace, path);
                if path.to_str() != Some(record.source_path.as_str()) {
                    return Err(INPUT_MISMATCH);
                }
                let name = local_input_filename(&path).map_err(|_| INPUT_MISMATCH)?;
                (
                    open_local_input(&path).map_err(|_| INPUT_MISMATCH)?,
                    name,
                    None,
                )
            };
            let data_dir = self.config.data_dir.clone();
            let expected_size = record.byte_size;
            let (snapshot, length, digest) = media::blocking(move || {
                capture_input(
                    &data_dir,
                    source,
                    &filename,
                    expected_size,
                    media_type.as_deref(),
                )
            })
            .await
            .map_err(|_| INPUT_MISMATCH)?;
            if record.sha256 != digest || record.byte_size != length {
                return Err(INPUT_MISMATCH);
            }
            verified.push(VerifiedInput { record, snapshot });
        }
        Ok(verified)
    }

    async fn upload_bound_images(
        &self,
        loaded: &LoadedWorkflow,
        bound: &mut BoundWorkflow,
        stored: Option<BTreeMap<String, UploadMetadata>>,
        snapshots: Option<Vec<VerifiedInput>>,
        deadline: &CancellationToken,
    ) -> Result<Vec<InputFileRecord>> {
        let mut source_inputs = Vec::new();
        if let Some(snapshots) = snapshots {
            for input in snapshots {
                source_inputs.push(self.upload_input_snapshot(input, deadline).await?);
            }
        } else if let Some(stored) = stored {
            let store = self.uploads();
            for (parameter, metadata) in stored {
                let filename = format!("genaimage-{}.{}", Uuid::new_v4(), metadata.extension());
                let upload = before_deadline(deadline, async {
                    let bytes = store.verified_bytes(&metadata).await?;
                    self.backend
                        .upload_image_bytes(bytes, &filename, &metadata.media_type)
                        .await
                })
                .await;
                let record = InputFileRecord {
                    parameter,
                    source_path: String::new(),
                    upload_id: Some(metadata.upload_id),
                    sha256: metadata.sha256,
                    original_filename: Some(metadata.original_filename),
                    byte_size: metadata.byte_size,
                    uploaded_name: String::new(),
                };
                source_inputs.push(record_uploaded_input(record, upload)?);
            }
        } else {
            for (parameter, path) in &bound.image_uploads {
                let path = resolve_input_path(&self.config.workspace, path);
                let parameter = parameter.clone();
                let data_dir = self.config.data_dir.clone();
                let input =
                    media::blocking(move || capture_local_input(&data_dir, parameter, &path))
                        .await?;
                source_inputs.push(self.upload_input_snapshot(input, deadline).await?);
            }
        }
        let names = source_inputs
            .iter()
            .map(|record| (record.parameter.clone(), record.uploaded_name.clone()))
            .collect();
        loaded.apply_uploaded_images(bound, &names)?;
        Ok(source_inputs)
    }

    async fn upload_input_snapshot(
        &self,
        input: VerifiedInput,
        deadline: &CancellationToken,
    ) -> Result<InputFileRecord> {
        let upload =
            before_deadline(deadline, self.backend.upload_image(&input.snapshot.path)).await;
        record_uploaded_input(input.record, upload)
    }

    async fn submit_inner(
        &self,
        submission: SubmissionRequest,
        keyed: Option<(String, String)>,
        batch: Option<BatchLink>,
        deadline: &CancellationToken,
    ) -> Result<KeyedSubmission> {
        let repository = self.repository.as_async();
        let (request, source) = match submission {
            SubmissionRequest::New(request) => (request, None),
            SubmissionRequest::Rerun(id) => {
                let run = repository.get(&id).await?;
                let (manifest_hash, provenance) = self.rerun_provenance(&run)?;
                if provenance.parameters != run.parameters {
                    return Err(MISSING_PROVENANCE);
                }
                let request = RunRequest {
                    workflow: run.workflow_id.clone(),
                    preset: provenance.preset,
                    parameters: serde_json::from_value(run.parameters.clone())
                        .map_err(|_| MISSING_PROVENANCE)?,
                    version: None,
                    workflow_revision: None,
                };
                (
                    request,
                    Some((run, manifest_hash, provenance.source_inputs)),
                )
            }
        };
        let loaded = if let Some((run, manifest_hash, _)) = &source {
            if !single_path_component(&request.workflow) {
                return Err(WORKFLOW_MISMATCH);
            }
            let loaded = self
                .load_workflow_by_id(&request.workflow)
                .map_err(|error| match error {
                    error @ (HarnessError::WorkflowDiscoveryIncomplete { .. }
                    | HarnessError::WorkflowAmbiguous(_)) => error,
                    _ => WORKFLOW_MISMATCH,
                })?
                .ok_or(WORKFLOW_MISMATCH)?;
            if loaded.manifest.version != run.workflow_version
                || loaded.revision() != workflow_revision(&run.workflow_sha256, manifest_hash)
            {
                return Err(WORKFLOW_MISMATCH);
            }
            loaded
        } else if keyed.is_some() {
            if !single_path_component(&request.workflow) {
                return Err(HarnessError::InvalidRunRequest(
                    "Select a configured workflow by ID, not by path.".into(),
                ));
            }
            if request.version.as_deref().is_some_and(|version| {
                !single_path_component(version)
                    || !version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            }) {
                return Err(HarnessError::InvalidRunRequest("Use a version containing only letters, digits, hyphens, underscores or dots, other than '.' and '..'.".into()));
            }
            self.load_api_workflow(&request.workflow, request.workflow_revision.as_deref())?
                .ok_or_else(|| {
                    HarnessError::Workflow(format!("workflow '{}' was not found", request.workflow))
                })?
        } else {
            let loaded = self.load_workflow(&request.workflow)?;
            loaded.verify_revision(request.workflow_revision.as_deref())?;
            loaded
        };
        let stored = if keyed.is_some() && source.is_none() {
            let (errors, uploads) = self
                .check_api_bindings(&loaded, request.preset.as_deref(), &request.parameters)
                .await?;
            if !errors.is_empty() {
                return Err(HarnessError::Workflow(
                    errors
                        .into_iter()
                        .map(|error| format!("{}: {}", error.parameter, error.reason))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            Some(uploads)
        } else {
            None
        };
        let mut bound = loaded
            .bind(request.preset.as_deref(), &request.parameters)
            .map_err(|error| {
                if source.is_some() {
                    WORKFLOW_MISMATCH
                } else {
                    error
                }
            })?;
        let parameters = serde_json::to_value(&bound.parameters)?;
        let (rerun_of, snapshots) = if let Some((run, _, inputs)) = source {
            if parameters != run.parameters {
                return Err(WORKFLOW_MISMATCH);
            }
            verify_loader_bindings(&loaded, &bound)?;
            let snapshots =
                before_deadline(deadline, self.verified_rerun_inputs(&bound, inputs)).await?;
            (Some(run.id), Some(snapshots))
        } else {
            (None, None)
        };
        before_deadline(deadline, self.validate_workflow_nodes(&bound.workflow)).await?;

        let run_id = format!("run-{}", Uuid::new_v4());
        let prompt_id = Uuid::new_v4().to_string();
        let client_id = format!("genaimage-{}", Uuid::new_v4());
        let output_dir = self.output_directory(&loaded, request.version.as_deref(), &run_id)?;
        let run_guard = self
            .events
            .run_lock(&self.database_identity, &run_id, RunLockKind::Reconcile)
            .lock_owned()
            .await;
        let owner = self.try_claim_submission(&run_id)?.ok_or_else(|| {
            HarnessError::Database("new run already has a submission owner".into())
        })?;
        let new_run = NewRun {
            id: run_id.clone(),
            prompt_id: prompt_id.clone(),
            client_id: client_id.clone(),
            workflow_id: loaded.manifest.id.clone(),
            workflow_version: loaded.manifest.version.clone(),
            workflow_sha256: loaded.workflow_sha256.clone(),
            workflow_manifest_sha256: Some(loaded.manifest_sha256.clone()),
            rerun_of,
            generation_mode: GenerationMode::Generic,
            status: RunStatus::Submitting,
            parameters: parameters.clone(),
            output_dir: output_dir.display().to_string(),
        };
        let is_keyed = keyed.is_some();
        let prepare_files = async {
            let source_inputs = self
                .upload_bound_images(&loaded, &mut bound, stored, snapshots, deadline)
                .await?;
            if is_keyed {
                fs::create_dir_all(self.config.runs.join(&loaded.manifest.id))?;
                fs::create_dir(&output_dir)?;
            } else {
                fs::create_dir_all(&output_dir)?;
            }
            artifacts::initialise_run_files(
                &output_dir,
                &bound,
                &loaded,
                request.preset.as_deref(),
                &parameters,
                &source_inputs,
            )
        };
        let (run, (preflight, resolved_checksum)) = if let Some((key, fingerprint)) = keyed {
            let run = match repository
                .reserve_submission(key, fingerprint, new_run, batch)
                .await?
            {
                SubmissionReservation::Existing(run) => {
                    return Ok(KeyedSubmission {
                        run: self.recover_submission(run).await?.0,
                        replayed: true,
                    })
                }
                SubmissionReservation::New(run) => run,
            };
            // The mapping is already durable: never release it, even after a local failure.
            let preflight = match prepare_files.await {
                Ok(preflight) => preflight,
                Err(error) => {
                    let message = error.to_string();
                    let Some((run, transition)) = self
                        .transition_to_terminal(&run.id, RunStatus::Failed, Some(&message), None)
                        .await?
                    else {
                        return Ok(KeyedSubmission {
                            run: repository.get(&run.id).await?,
                            replayed: false,
                        });
                    };
                    repository
                        .record_event(&run.id, "preparation_failed", Some(&message), &Value::Null)
                        .await?;
                    transition.publish(Event::RunFailed { run: run.clone() });
                    return Ok(KeyedSubmission {
                        run,
                        replayed: false,
                    });
                }
            };
            (run, preflight)
        } else {
            let preflight = prepare_files.await?;
            (
                repository.insert_owned_submission(new_run).await?,
                preflight,
            )
        };
        repository
            .record_preflight(run.id.clone(), preflight, resolved_checksum)
            .await?;
        repository
            .record_event(
                &run.id,
                "run_created",
                Some("Run files and preflight validation completed"),
                &json!({
                    "manifest_path": loaded.manifest_path,
                    "preset": request.preset
                }),
            )
            .await?;

        repository
            .record_event(
                &run.id,
                "submission_attempted",
                None,
                &json!({"timeout_seconds": self.config.comfyui.request_timeout_seconds}),
            )
            .await?;
        let response = tokio::select! {
            biased;
            _ = deadline.cancelled() => Err(HarnessError::ShuttingDown),
            response = self.backend.submit(&bound.workflow, &client_id, &prompt_id) => response,
        };
        let harness = self.clone();
        let run = tokio::spawn(async move {
            let (_guard, _owner) = (run_guard, owner);
            harness.finish_submission(run, response).await
        })
        .await
        .map_err(transition_task_error)??;
        Ok(KeyedSubmission {
            run,
            replayed: false,
        })
    }

    async fn finish_submission(
        &self,
        run: RunRecord,
        response: Result<PromptResponse>,
    ) -> Result<RunRecord> {
        let response =
            match response {
                Ok(response) if !response.prompt_id.trim().is_empty() => response,
                Ok(_) => {
                    return self
                        .mark_submission_unknown(
                            &run.id,
                            "The backend response did not contain a prompt ID.",
                        )
                        .await
                }
                Err(HarnessError::ShuttingDown) => return self
                    .mark_submission_unknown(
                        &run.id,
                        "Shutdown deadline expired before the prompt acknowledgement was recorded.",
                    )
                    .await,
                Err(error) => {
                    return self
                        .mark_submission_unknown(&run.id, &error.to_string())
                        .await
                }
            };
        let warnings = response
            .node_errors
            .as_object()
            .is_some_and(|errors| !errors.is_empty())
            || response
                .node_errors
                .as_array()
                .is_some_and(|errors| !errors.is_empty());
        let repository = self.repository.as_async();
        let transition = TransitionGuard::new(&self.events);
        let Some(run) = repository
            .acknowledge_submission(
                &run.id,
                response.prompt_id.clone(),
                warnings.then(|| response.node_errors.clone()),
            )
            .await?
        else {
            transition.disarm();
            return repository.get(&run.id).await;
        };
        let message = if warnings {
            " accepted the prompt with node warnings"
        } else {
            " accepted the prompt"
        };
        let queued = run.status == RunStatus::Queued;
        repository
            .record_event(
                &run.id,
                if queued {
                    "prompt_queued"
                } else {
                    "submission_acknowledged"
                },
                Some(&format!("{}{message}", self.backend.name())),
                &serde_json::to_value(response)?,
            )
            .await?;
        if queued {
            transition.publish(Event::RunQueued { run: run.clone() });
        } else {
            transition.publish(Event::RunProgress {
                run: run.clone(),
                backend_event: None,
            });
        }
        Ok(run)
    }

    async fn mark_submission_unknown(&self, id: &str, message: &str) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let transition = TransitionGuard::new(&self.events);
        let Some(run) = repository
            .compare_and_set_status(
                id,
                RunStatus::Submitting,
                RunStatus::UnknownSubmission,
                Some(message),
                None,
            )
            .await?
        else {
            transition.disarm();
            return repository.get(id).await;
        };
        repository
            .record_event(
                id,
                "submission_unknown",
                Some("Submission was not automatically retried"),
                &json!({"error": message}),
            )
            .await?;
        transition.publish(Event::RunUnknownSubmission { run: run.clone() });
        Ok(run)
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunRecord> {
        self.repository.get(run_id)
    }

    pub fn list_runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        self.repository.list(limit)
    }

    pub fn run_events(&self, run_id: &str, limit: usize) -> Result<Vec<RunEvent>> {
        self.repository.events(run_id, limit)
    }

    pub async fn reconcile(&self, run_id: &str) -> Result<ReconcileResult> {
        self.reconcile_in_task(run_id, true).await
    }

    async fn reconcile_in_task(&self, run_id: &str, audit: bool) -> Result<ReconcileResult> {
        let harness = self.clone();
        let run_id = run_id.to_owned();
        tokio::spawn(async move {
            harness
                .reconcile_with_audit(&run_id, &CancellationToken::new(), audit, &|_| {})
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Reconcile within the caller's task, stopping backend I/O at the deadline.
    pub async fn reconcile_until(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
    ) -> Result<ReconcileResult> {
        self.reconcile_until_with_active(run_id, deadline, &|_| {})
            .await
    }

    /// Notify the caller once queue evidence is reflected in an active run record,
    /// before any later history or audit error can prevent observation.
    pub async fn reconcile_until_with_active(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
        on_active: &(dyn Fn(&RunRecord) + Sync),
    ) -> Result<ReconcileResult> {
        self.reconcile_with_audit(run_id, deadline, true, on_active)
            .await
    }

    async fn reconcile_with_audit(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
        audit: bool,
        on_active: &(dyn Fn(&RunRecord) + Sync),
    ) -> Result<ReconcileResult> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Reconcile);
        let _guard = before_deadline(deadline, async { Ok(lock.lock().await) }).await?;
        let (run, outcome, settles_at) = self.inspect_run(run_id, deadline, on_active).await?;
        let repository = self.repository.as_async();
        let outputs = repository.outputs(run_id).await?;
        // Polling uses the same inspection without writing an event every second.
        if audit {
            repository
                .record_event(
                    run_id,
                    "run_reconciled",
                    None,
                    &json!({ "outcome": outcome }),
                )
                .await?;
        }
        Ok(ReconcileResult {
            run,
            outputs,
            outcome,
            settles_at,
        })
    }

    async fn inspect_run(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
        on_active: &(dyn Fn(&RunRecord) + Sync),
    ) -> Result<(RunRecord, ReconcileOutcome, Option<DateTime<Utc>>)> {
        let repository = self.repository.as_async();
        let (mut run, live_owner) = self
            .recover_submission(repository.get(run_id).await?)
            .await?;
        if run.status.is_terminal() {
            return Ok((run, ReconcileOutcome::Terminal, None));
        }
        if live_owner {
            return Ok((run, ReconcileOutcome::InProgressElsewhere, None));
        }
        let prompt_id = run.prompt_id.clone();
        let observed_at = Utc::now();
        let queue = before_deadline(deadline, self.backend.queue()).await?;
        let queued = queue.contains(&prompt_id);
        let transition_status = if queue.is_running(&prompt_id) {
            (!matches!(run.status, RunStatus::Running | RunStatus::Cancelling))
                .then_some(RunStatus::Running)
        } else if queued && !matches!(run.status, RunStatus::Queued | RunStatus::Cancelling) {
            Some(RunStatus::Queued)
        } else {
            None
        };
        let mut notified = false;
        if let Some(status) = transition_status {
            let transition = TransitionGuard::new(&self.events);
            if let Some(updated) = repository
                .compare_and_set_status(run_id, run.status, status, None, None)
                .await?
            {
                run = updated;
                on_active(&run);
                notified = true;
                if status == RunStatus::Running {
                    repository
                        .record_event(
                            run_id,
                            "prompt_running",
                            Some(&format!(
                                "{} reports the prompt as executing",
                                self.backend.name()
                            )),
                            &serde_json::to_value(&queue)?,
                        )
                        .await?;
                    transition.publish(Event::RunStarted { run: run.clone() });
                } else {
                    repository
                        .record_event(
                            run_id,
                            "prompt_queued",
                            Some(&format!(
                                "{} reports the prompt in the pending queue",
                                self.backend.name()
                            )),
                            &json!({"prompt_id": run.prompt_id}),
                        )
                        .await?;
                    transition.publish(Event::RunQueued { run: run.clone() });
                }
            } else {
                transition.disarm();
                run = repository.get(run_id).await?;
                if run.status.is_terminal() {
                    return Ok((run, ReconcileOutcome::Terminal, None));
                }
            }
        }

        if !notified
            && queued
            && run.prompt_id == prompt_id
            && matches!(
                run.status,
                RunStatus::Queued | RunStatus::Running | RunStatus::Cancelling
            )
        {
            on_active(&run);
        }

        let history = before_deadline(deadline, self.backend.history(&run.prompt_id)).await?;
        if history.prompt_id != run.prompt_id {
            return Err(self.backend_error(format!(
                "requested history for prompt '{}', but received history for '{}'",
                run.prompt_id, history.prompt_id
            )));
        }
        match history.outcome {
            HistoryOutcome::NotFound | HistoryOutcome::Unresolved => {}
            HistoryOutcome::Completed if history.outputs.is_empty() => {}
            HistoryOutcome::Cancelled => {
                let message = format!(
                    "{} confirmed cancellation of the prompt",
                    self.backend.name()
                );
                if let Some((updated, transition)) = self
                    .transition_to_terminal(run_id, RunStatus::Cancelled, Some(&message), None)
                    .await?
                {
                    repository
                        .record_event(run_id, "run_cancelled", Some(&message), &history.raw)
                        .await?;
                    transition.publish(Event::RunCancelled { run: updated });
                }
            }
            HistoryOutcome::Completed => {
                let source_inputs = read_source_inputs(&PathBuf::from(&run.output_dir))?;
                let workflow_manifest_path = read_manifest_path(&PathBuf::from(&run.output_dir))?;
                let manifest_path = artifacts::materialise_history(
                    self.backend.as_ref(),
                    &repository,
                    &run,
                    &workflow_manifest_path,
                    source_inputs,
                    &history,
                    deadline,
                )
                .await?;
                if let Some((updated, transition)) = self
                    .transition_to_terminal(
                        run_id,
                        RunStatus::Completed,
                        None,
                        Some(&manifest_path.display().to_string()),
                    )
                    .await?
                {
                    for output in repository.outputs(run_id).await? {
                        self.events.publish(Event::RunOutput {
                            run_id: run_id.to_owned(),
                            output,
                        });
                    }
                    repository.record_event(
                        run_id, "run_completed", Some("Outputs and provenance manifest were captured"),
                        &json!({"output_count": history.outputs.len(), "manifest_path": manifest_path}),
                    ).await?;
                    transition.publish(Event::RunCompleted { run: updated });
                }
            }
            HistoryOutcome::Failed => {
                let error = format!(
                    "{} completed prompt '{}' without a successful result",
                    self.backend.name(),
                    run.prompt_id
                );
                if let Some((updated, transition)) = self
                    .transition_to_terminal(run_id, RunStatus::Failed, Some(&error), None)
                    .await?
                {
                    repository
                        .record_event(run_id, "run_failed", Some(&error), &history.raw)
                        .await?;
                    transition.publish(Event::RunFailed { run: updated });
                }
            }
        }
        let run = repository.get(run_id).await?;
        let mut settles_at = None;
        let outcome = if run.status.is_terminal() {
            ReconcileOutcome::Terminal
        } else if prompt_id != run.prompt_id {
            ReconcileOutcome::StillAmbiguous
        } else if queued
            && matches!(
                run.status,
                RunStatus::Queued | RunStatus::Running | RunStatus::Cancelling
            )
        {
            ReconcileOutcome::Active
        } else if !queued
            && run.status == RunStatus::UnknownSubmission
            && history.outcome == HistoryOutcome::NotFound
        {
            let eligible_at = repository.submission_attempt(run_id).await?.and_then(
                |(attempted_at, timeout_seconds)| {
                    timeout_seconds
                        .checked_add(ADMISSION_GRACE_SECONDS)
                        .and_then(|seconds| i64::try_from(seconds).ok())
                        .and_then(TimeDelta::try_seconds)
                        .and_then(|delta| attempted_at.checked_add_signed(delta))
                },
            );
            if eligible_at.is_some_and(|instant| instant <= observed_at) {
                ReconcileOutcome::NotFound
            } else {
                settles_at = eligible_at;
                ReconcileOutcome::StillAmbiguous
            }
        } else {
            ReconcileOutcome::StillAmbiguous
        };
        Ok((run, outcome, settles_at))
    }

    async fn transition_to_terminal(
        &self,
        run_id: &str,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<Option<(RunRecord, TransitionGuard)>> {
        let repository = self.repository.as_async();
        loop {
            let current = repository.get(run_id).await?;
            if current.status.is_terminal() {
                return Ok(None);
            }
            let guard = TransitionGuard::new(&self.events);
            match repository
                .compare_and_set_status(run_id, current.status, status, error, manifest_path)
                .await?
            {
                Some(updated) => return Ok(Some((updated, guard))),
                None => guard.disarm(),
            }
        }
    }

    pub async fn reconcile_all(&self) -> Result<Vec<ReconcileResult>> {
        let repository = self.repository.as_async();
        let mut results = Vec::new();
        for run in repository.active().await? {
            results.push(self.reconcile(&run.id).await?);
        }
        Ok(results)
    }

    /// Reconcile all active runs in the caller's task, with one shared deadline.
    pub async fn reconcile_all_until(
        &self,
        deadline: &CancellationToken,
    ) -> Result<Vec<ReconcileResult>> {
        self.reconcile_all_until_with_active(deadline, &|_| {})
            .await
    }

    /// Notify per run, including when another run later fails reconciliation.
    pub async fn reconcile_all_until_with_active(
        &self,
        deadline: &CancellationToken,
        on_active: &(dyn Fn(&RunRecord) + Sync),
    ) -> Result<Vec<ReconcileResult>> {
        let mut results = Vec::new();
        for run in self.repository.as_async().active().await? {
            results.push(
                self.reconcile_until_with_active(&run.id, deadline, on_active)
                    .await?,
            );
        }
        Ok(results)
    }

    pub async fn wait_for_run(&self, run_id: &str) -> Result<RunRecord> {
        self.wait_for_run_inner(run_id, None).await
    }

    /// Stop observing when draining starts, and stop an in-flight poll at the deadline.
    pub async fn monitor_run(
        &self,
        run_id: &str,
        stop: &CancellationToken,
        deadline: &CancellationToken,
    ) -> Result<()> {
        match self
            .wait_for_run_inner(run_id, Some((stop, deadline)))
            .await
        {
            Ok(_) | Err(HarnessError::ShuttingDown) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn wait_for_run_inner(
        &self,
        run_id: &str,
        stop: Option<(&CancellationToken, &CancellationToken)>,
    ) -> Result<RunRecord> {
        let repository = self.repository.as_async();
        let initial = repository.get(run_id).await?;
        let stopped = async {
            match stop {
                Some((stop, _)) => stop.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(stopped);
        let mut monitor = tokio::select! {
            biased;
            _ = &mut stopped => return Ok(initial),
            result = self.backend.monitor_events(&initial.client_id) => result?,
        };
        let mut poll = interval(Duration::from_secs(1));
        poll.tick().await;
        let mut events_open = true;
        loop {
            tokio::select! {
                _ = &mut stopped => return repository.get(run_id).await,
                event = monitor.recv(), if events_open => {
                    if let Some(event) = event {
                        let relevant = event.prompt_id.as_deref().is_none()
                            || event.prompt_id.as_deref() == Some(initial.prompt_id.as_str());
                        if relevant {
                            repository.record_event(
                                run_id,
                                &event.event_type,
                                event.message.as_deref(),
                                &serde_json::to_value(&event)?,
                            ).await?;
                            let run = repository.get(run_id).await?;
                            self.events.publish(Event::RunProgress { run, backend_event: Some(event) });
                        }
                    } else {
                        events_open = false;
                    }
                }
                _ = poll.tick() => {
                    let result = if let Some((_, deadline)) = stop {
                        self.reconcile_with_audit(run_id, deadline, false, &|_| {}).await?
                    } else {
                        self.reconcile_in_task(run_id, false).await?
                    };
                    self.events.publish(Event::RunProgress { run: result.run.clone(), backend_event: None });
                    if result.run.status.is_terminal() {
                        return Ok(result.run);
                    }
                }
            }
        }
    }

    pub async fn cancel(&self, run_id: &str) -> Result<RunRecord> {
        let harness = self.clone();
        let run_id = run_id.to_owned();
        tokio::spawn(async move {
            harness
                .cancel_until(&run_id, &CancellationToken::new())
                .await
        })
        .await
        .map_err(transition_task_error)?
    }

    /// Cancel within the caller's task; only backend I/O is cut off at the deadline.
    pub async fn cancel_until(
        &self,
        run_id: &str,
        deadline: &CancellationToken,
    ) -> Result<RunRecord> {
        let lock = self
            .events
            .run_lock(&self.database_identity, run_id, RunLockKind::Cancel);
        let _guard = before_deadline(deadline, async { Ok(lock.lock().await) }).await?;
        let repository = self.repository.as_async();
        let run = repository.get(run_id).await?;
        if run.status.is_terminal() {
            return Err(HarnessError::RunConflict(format!(
                "run '{run_id}' is already {} and cannot be cancelled",
                run.status.as_str()
            )));
        }
        let queue = before_deadline(deadline, self.backend.queue()).await?;
        let running = queue.is_running(&run.prompt_id);
        if !running && !queue.contains(&run.prompt_id) {
            return Err(HarnessError::RunConflict(format!(
                "prompt '{}' is not in the local {} queue; reconcile the run first",
                run.prompt_id,
                self.backend.name()
            )));
        }
        let previous = loop {
            let current = repository.get(run_id).await?;
            if current.status.is_terminal() {
                return Err(HarnessError::RunConflict(format!(
                    "run '{run_id}' is already {} and cannot be cancelled",
                    current.status.as_str()
                )));
            }
            if repository
                .compare_and_set_status(run_id, current.status, RunStatus::Cancelling, None, None)
                .await?
                .is_some()
            {
                break current.status;
            }
        };
        let preparation = if running {
            before_deadline(deadline, self.backend.prepare_interrupt(&run.prompt_id)).await
        } else {
            Ok(())
        };
        if let Err(error) = preparation {
            self.finish_unsent_cancellation(run_id, previous, &error)
                .await?;
            return Err(error);
        }
        let mut attempted = false;
        let response = before_deadline(deadline, async {
            attempted = true;
            if running {
                self.backend.interrupt(&run.prompt_id).await
            } else {
                self.backend.delete_queued(&run.prompt_id).await
            }
        })
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                match &error {
                    HarnessError::ShuttingDown if !attempted => {
                        self.finish_unsent_cancellation(run_id, previous, &error)
                            .await?;
                    }
                    HarnessError::CancellationNotSent(_) => {
                        self.finish_unsent_cancellation(run_id, previous, &error)
                            .await?;
                    }
                    HarnessError::CancellationRejected(_) => {
                        repository
                            .rollback_cancellation(run_id, previous, error.to_string())
                            .await?;
                    }
                    _ => {
                        let cause = if matches!(error, HarnessError::ShuttingDown) {
                            "The shutdown deadline expired while waiting for cancellation acknowledgement.".into()
                        } else {
                            error.to_string()
                        };
                        repository.record_event(run_id, "cancellation_unknown",
                            Some("Cancellation was not confirmed; reconcile the run before taking further action."),
                            &json!({"prompt_id": run.prompt_id, "error": cause})).await?;
                        if !matches!(error, HarnessError::ShuttingDown) {
                            return Err(HarnessError::CancellationUnknown(cause));
                        }
                    }
                }
                return Err(error);
            }
        };
        let (event_type, message) = if running {
            (
                "interrupt_requested",
                format!(
                    "{} was asked to interrupt the executing prompt",
                    self.backend.name()
                ),
            )
        } else {
            (
                "delete_requested",
                format!(
                    "{} was asked to remove the queued prompt",
                    self.backend.name()
                ),
            )
        };
        repository
            .record_event(run_id, event_type, Some(&message), &response)
            .await?;
        repository.get(run_id).await
    }

    async fn finish_unsent_cancellation(
        &self,
        id: &str,
        previous: RunStatus,
        error: &HarnessError,
    ) -> Result<()> {
        let repository = self.repository.as_async();
        repository
            .rollback_cancellation(id, previous, error.to_string())
            .await?;
        repository
            .record_event(
                id,
                "cancellation_not_sent",
                Some("The cancellation request was not sent."),
                &json!({"error": error.to_string()}),
            )
            .await
    }

    fn output_directory(
        &self,
        workflow: &LoadedWorkflow,
        version: Option<&str>,
        run_id: &str,
    ) -> Result<PathBuf> {
        let version = version.unwrap_or(run_id);
        let safe_version: String = version
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        if safe_version.is_empty() {
            return Err(HarnessError::Configuration(
                "output version cannot be empty".to_owned(),
            ));
        }
        Ok(self
            .config
            .runs
            .join(&workflow.manifest.id)
            .join(safe_version))
    }
}

fn single_path_component(value: &str) -> bool {
    !value.is_empty() && !matches!(value, "." | "..") && !value.contains(['/', '\\', '\0'])
}

fn resolve_input_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn upload_failure(parameter: &str, error: HarnessError) -> HarnessError {
    HarnessError::Workflow(format!(
        "Reference upload for parameter '{parameter}' failed before prompt submission: {error}"
    ))
}

fn open_local_input(path: &Path) -> Result<File> {
    let target = path.canonicalize()?;
    target
        .parent()
        .zip(target.file_name())
        .and_then(|(parent, name)| {
            crate::safe_file::open_regular_beneath(parent, parent, Path::new(name))
        })
        .ok_or(HarnessError::MediaIntegrity)
}

fn local_input_filename(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| HarnessError::InvalidPath {
            path: path.to_path_buf(),
            reason: "file name is not valid UTF-8".into(),
        })
}

fn capture_input(
    data: &Path,
    source: File,
    filename: &str,
    size: u64,
    media_type: Option<&str>,
) -> Result<(InputSnapshot, u64, String)> {
    fs::create_dir_all(data)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("input-snapshot-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let directory = builder.tempdir_in(data)?;
    let path = directory.path().join(filename);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut copy = media::copy_snapshot(source, file, Some(size))?;
    if copy.len != size {
        return Err(HarnessError::MediaIntegrity);
    }
    if let Some(media_type) = media_type {
        let mut header = Vec::new();
        (&mut copy.file).take(12).read_to_end(&mut header)?;
        if detected_type(&header) != Some(media_type) {
            return Err(HarnessError::MediaIntegrity);
        }
    }
    Ok((
        InputSnapshot {
            _directory: directory,
            path,
        },
        copy.len,
        copy.sha256,
    ))
}

fn capture_local_input(data: &Path, parameter: String, path: &Path) -> Result<VerifiedInput> {
    let filename = local_input_filename(path)?;
    let source = open_local_input(path)?;
    let size = source.metadata()?.len();
    let (snapshot, byte_size, sha256) = capture_input(data, source, &filename, size, None)?;
    Ok(VerifiedInput {
        record: InputFileRecord {
            parameter,
            source_path: path.display().to_string(),
            upload_id: None,
            original_filename: None,
            sha256,
            byte_size,
            uploaded_name: String::new(),
        },
        snapshot,
    })
}

fn record_uploaded_input(
    mut record: InputFileRecord,
    upload: Result<UploadResponse>,
) -> Result<InputFileRecord> {
    let upload = upload.map_err(|error| upload_failure(&record.parameter, error))?;
    if upload.kind != "input"
        || !upload.subfolder.is_empty()
        || !single_path_component(&upload.name)
        || upload.name.contains([':', '[', ']'])
        || upload.name.chars().any(char::is_control)
    {
        return Err(HarnessError::Workflow(format!("The backend returned an invalid input filename for parameter '{}'; no prompt was submitted.", record.parameter)));
    }
    record.uploaded_name = upload.name;
    Ok(record)
}

fn verify_loader_bindings(loaded: &LoadedWorkflow, bound: &BoundWorkflow) -> Result<()> {
    let slots: BTreeSet<_> = bound
        .image_uploads
        .keys()
        .map(|parameter| {
            let definition = &loaded.manifest.parameters[parameter];
            (definition.node.as_str(), definition.input.as_str())
        })
        .collect();
    // An upload binding replaces the actual loader slot after verification.
    // A filename, a linked path, or a binding on some other slot does not.
    let nodes = bound.workflow.as_object().ok_or(WORKFLOW_MISMATCH)?;
    for (node, value) in nodes {
        let input = match value.get("class_type").and_then(Value::as_str) {
            Some("LoadImage" | "LoadImageMask" | "LoadImageOutput") => "image",
            Some("LoadAudio") => "audio",
            Some("LoadVideo") => "file",
            Some("LoadLatent") => "latent",
            _ => continue,
        };
        if !slots.contains(&(node.as_str(), input)) {
            return Err(HarnessError::RerunUnverifiedMedia {
                node: node.clone(),
                input,
            });
        }
    }
    Ok(())
}

fn read_source_inputs(output_dir: &Path) -> Result<Vec<InputFileRecord>> {
    let value: Value = serde_json::from_slice(&fs::read(output_dir.join("preflight.json"))?)?;
    serde_json::from_value(
        value
            .get("source_inputs")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(HarnessError::Json)
}

fn read_manifest_path(output_dir: &Path) -> Result<PathBuf> {
    let value: Value = serde_json::from_slice(&fs::read(output_dir.join("preflight.json"))?)?;
    value
        .get("workflow_manifest_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            HarnessError::Workflow("preflight.json is missing workflow_manifest_path".to_owned())
        })
}

fn sidecar_source(path: &Path) -> Option<Value> {
    read_regular_file(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn sidecar_declares_id(path: &Path, id: &str) -> bool {
    sidecar_source(path)
        .is_some_and(|manifest| manifest.get("id").and_then(Value::as_str) == Some(id))
}

enum WorkflowVisit {
    Manifest(Result<Box<LoadedWorkflow>>),
    Diagnostic(std::io::Error),
    Incomplete(std::io::Error),
}

enum FileVisit {
    File,
    Diagnostic(std::io::Error),
    Incomplete(std::io::Error),
}

fn visit_workflows(
    directory: &Path,
    pack_roots: &[PathBuf],
    mut visitor: impl FnMut(&Path, WorkflowVisit),
) {
    let pack_roots: Vec<PathBuf> = pack_roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect();
    visit_files(
        directory,
        &pack_roots,
        &mut Vec::new(),
        &mut |path, visit| match visit {
            FileVisit::Incomplete(error) => visitor(path, WorkflowVisit::Incomplete(error)),
            FileVisit::Diagnostic(error) => visitor(path, WorkflowVisit::Diagnostic(error)),
            FileVisit::File
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".genaimage.json")) =>
            {
                match LoadedWorkflow::load(path) {
                    Err(HarnessError::Io(error)) => visitor(path, WorkflowVisit::Incomplete(error)),
                    result => visitor(path, WorkflowVisit::Manifest(result.map(Box::new))),
                }
            }
            FileVisit::File => {}
        },
    );
}

fn broken_symlink(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
        return true;
    }
    false
}

fn visit_files(
    directory: &Path,
    pack_roots: &[PathBuf],
    ancestors: &mut Vec<PathBuf>,
    visitor: &mut dyn FnMut(&Path, FileVisit),
) {
    let canonical = match fs::canonicalize(directory) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && ancestors.is_empty() => {
            return
        }
        Err(error) => {
            visitor(directory, FileVisit::Incomplete(error));
            return;
        }
    };
    if ancestors.contains(&canonical) {
        visitor(
            directory,
            FileVisit::Diagnostic(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "workflow directory contains a symlink cycle",
            )),
        );
        return;
    }
    let pack_file = directory.join(crate::pack::PACK_FILE);
    if pack_file.is_file() {
        // A pack inside a configured pack root owns its directory; scanning it
        // here would report its workflows twice. Anywhere else the workflows
        // stay visible and the stray pack is explained.
        if pack_roots.iter().any(|root| canonical.starts_with(root)) {
            return;
        }
        visitor(
            &pack_file,
            FileVisit::Diagnostic(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "this directory contains a pack.toml but is not inside a configured pack root; its pack is ignored. Move pack directories into `paths.packs` (see docs/packs.md)",
            )),
        );
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            visitor(directory, FileVisit::Incomplete(error));
            return;
        }
    };
    ancestors.push(canonical);
    for result in entries {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                visitor(directory, FileVisit::Incomplete(error));
                continue;
            }
        };
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                visitor(&path, FileVisit::Incomplete(error));
                continue;
            }
        };
        if kind.is_dir() {
            visit_files(&path, pack_roots, ancestors, visitor);
        } else if kind.is_symlink() {
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    visit_files(&path, pack_roots, ancestors, visitor)
                }
                Ok(_) => visitor(&path, FileVisit::File),
                Err(error) if broken_symlink(&error) => {
                    visitor(&path, FileVisit::Diagnostic(error))
                }
                Err(error) => visitor(&path, FileVisit::Incomplete(error)),
            }
        } else {
            visitor(&path, FileVisit::File);
        }
    }
    ancestors.pop();
}

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod tests;
