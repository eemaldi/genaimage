use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime},
};

use genaimage_core::{orchestrator::WorkflowDiscovery, Harness, HarnessError, LoadedWorkflow};
use tokio::sync::Mutex;

const REVALIDATE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    directory: bool,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Stamp {
    fn new(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            directory: metadata.is_dir(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    link: Option<Stamp>,
    target: Option<Stamp>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct ReferenceError {
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl From<io::Error> for ReferenceError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Fingerprint {
    entries: BTreeMap<PathBuf, Result<Entry, ReferenceError>>,
    references: BTreeMap<PathBuf, Result<Entry, ReferenceError>>,
    issues: Vec<(PathBuf, &'static str, ReferenceError)>,
}

fn metadata(path: &Path, follow: bool) -> io::Result<Option<Metadata>> {
    match if follow {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    } {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn entry(path: &Path) -> io::Result<Entry> {
    Ok(Entry {
        link: metadata(path, false)?
            .as_ref()
            .map(Stamp::new)
            .transpose()?,
        target: metadata(path, true)?.as_ref().map(Stamp::new).transpose()?,
    })
}

fn observe(path: &Path) -> Result<Entry, ReferenceError> {
    let state = entry(path).map_err(ReferenceError::from)?;
    if state.link.is_none() || state.target.is_none() {
        return Err(
            io::Error::new(io::ErrorKind::NotFound, "file or symlink target is missing").into(),
        );
    }
    Ok(state)
}

fn record_tree(
    path: &Path,
    entries: &mut BTreeMap<PathBuf, Result<Entry, ReferenceError>>,
    issues: &mut Vec<(PathBuf, &'static str, ReferenceError)>,
    ancestors: &mut Vec<PathBuf>,
) {
    let state = observe(path);
    let directory = state
        .as_ref()
        .is_ok_and(|state| state.target.as_ref().is_some_and(|stamp| stamp.directory));
    entries.insert(path.to_path_buf(), state);
    if !directory {
        return;
    }
    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) => {
            issues.push((path.to_path_buf(), "resolve directory", error.into()));
            return;
        }
    };
    if ancestors.contains(&canonical) {
        issues.push((
            path.to_path_buf(),
            "traverse directory",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "workflow directory contains a symlink cycle",
            )
            .into(),
        ));
        return;
    }
    let children = match fs::read_dir(path) {
        Ok(children) => children,
        Err(error) => {
            issues.push((path.to_path_buf(), "read directory", error.into()));
            return;
        }
    };
    ancestors.push(canonical);
    for item in children {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                issues.push((path.to_path_buf(), "read directory entry", error.into()));
                continue;
            }
        };
        let child = item.path();
        let kind = match item.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                entries.insert(child, Err(error.into()));
                continue;
            }
        };
        if kind.is_dir() {
            record_tree(&child, entries, issues, ancestors);
        } else if kind.is_symlink() {
            match observe(&child) {
                Ok(state) if state.target.as_ref().is_some_and(|stamp| stamp.directory) => {
                    record_tree(&child, entries, issues, ancestors)
                }
                state if state.is_err() || is_manifest(&child) => {
                    entries.insert(child, state);
                }
                _ => {}
            }
        } else if is_manifest(&child) {
            entries.insert(child.clone(), observe(&child));
        }
    }
    ancestors.pop();
}

fn is_manifest(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".genaimage.json"))
}

#[cfg(test)]
type DiscoveryHook = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
struct ScanProbe {
    #[cfg(test)]
    scans: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    after_discovery: Arc<std::sync::Mutex<Option<DiscoveryHook>>>,
}

fn fingerprint(root: &Path, references: &BTreeSet<PathBuf>, probe: &ScanProbe) -> Fingerprint {
    #[cfg(test)]
    probe
        .scans
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let _ = probe;
    let mut entries = BTreeMap::new();
    let mut issues = Vec::new();
    record_tree(root, &mut entries, &mut issues, &mut Vec::new());
    issues.sort_by(|left, right| {
        (&left.0, left.1, left.2.raw_os_error, &left.2.message).cmp(&(
            &right.0,
            right.1,
            right.2.raw_os_error,
            &right.2.message,
        ))
    });
    issues.dedup();
    let references = references
        .iter()
        .map(|path| (path.clone(), observe(path)))
        .collect();
    Fingerprint {
        entries,
        references,
        issues,
    }
}

fn append_diagnostics(discovery: &mut WorkflowDiscovery, fingerprint: &Fingerprint) {
    let entries = fingerprint
        .entries
        .iter()
        .chain(fingerprint.references.iter())
        .filter_map(|(path, result)| result.as_ref().err().map(|error| (path, error)));
    let issues = fingerprint
        .issues
        .iter()
        .map(|(path, _, error)| (path, error));
    for (path, error) in entries.chain(issues) {
        let path_text = path.to_string_lossy();
        if discovery.diagnostics.iter().any(|existing| {
            existing.path == *path
                || existing
                    .error
                    .to_string()
                    .contains(&format!("'{}'", path_text))
        }) {
            continue;
        }
        discovery
            .diagnostics
            .push(genaimage_core::orchestrator::WorkflowDiagnostic {
                path: path.clone(),
                error: HarnessError::Workflow(format!(
                    "Cannot inspect '{}': {}. Check the workflow files and permissions.",
                    path.display(),
                    error.message
                )),
            });
    }
    discovery
        .diagnostics
        .sort_by(|left, right| left.path.cmp(&right.path));
}

fn discover(harness: &Harness, probe: &ScanProbe) -> WorkflowDiscovery {
    #[cfg(test)]
    probe
        .scans
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let _ = probe;
    harness
        .discover_workflows()
        .unwrap_or_else(|error| WorkflowDiscovery {
            diagnostics: vec![genaimage_core::orchestrator::WorkflowDiagnostic {
                path: harness.config().workflows.clone(),
                error,
            }],
            ..WorkflowDiscovery::default()
        })
}

#[derive(Clone)]
pub(crate) struct ThumbnailSource {
    pub(crate) manifest_path: PathBuf,
    pub(crate) thumbnail: Option<String>,
}

struct IndexedWorkflow {
    source: ThumbnailSource,
    manifest: Option<Entry>,
}

pub(crate) struct WorkflowIndex {
    entries: BTreeMap<String, IndexedWorkflow>,
    blocked: BTreeSet<String>,
    traversal_failure: Option<genaimage_core::orchestrator::WorkflowTraversalFailure>,
    references: BTreeSet<PathBuf>,
    fingerprint: Option<Fingerprint>,
}

impl WorkflowIndex {
    fn references(discovery: &WorkflowDiscovery) -> BTreeSet<PathBuf> {
        discovery
            .referenced_workflow_paths
            .union(&discovery.referenced_thumbnail_paths)
            .cloned()
            .collect()
    }

    fn rebuild(harness: Arc<Harness>, probe: &ScanProbe) -> (Self, WorkflowDiscovery) {
        for attempt in 0..3 {
            let first = discover(&harness, probe);
            let before = fingerprint(
                &harness.config().workflows,
                &Self::references(&first),
                probe,
            );
            let mut discovery = discover(&harness, probe);
            #[cfg(test)]
            if let Some(hook) = probe.after_discovery.lock().unwrap().as_ref() {
                hook();
            }
            let after = fingerprint(
                &harness.config().workflows,
                &Self::references(&discovery),
                probe,
            );
            if before == after || attempt == 2 {
                if before != after {
                    discovery.diagnostics.push(genaimage_core::orchestrator::WorkflowDiagnostic {
                        path: harness.config().workflows.clone(),
                        error: HarnessError::Workflow("Workflow files changed during discovery; results may be incomplete. Try again when they are stable.".into()),
                    });
                }
                append_diagnostics(&mut discovery, &after);
                let mut entries = BTreeMap::new();
                for manifest in &discovery.manifests {
                    if let Some(path) = discovery.indexable_manifest_paths.get(&manifest.id) {
                        let graph = path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(&manifest.workflow);
                        if before != after
                            && (before.entries.get(path) != after.entries.get(path)
                                || before.references.get(&graph) != after.references.get(&graph))
                        {
                            continue;
                        }
                        let state = after
                            .entries
                            .get(path)
                            .and_then(|state| state.as_ref().ok())
                            .cloned();
                        entries.insert(
                            manifest.id.clone(),
                            IndexedWorkflow {
                                source: ThumbnailSource {
                                    manifest_path: path.clone(),
                                    thumbnail: manifest.thumbnail.clone(),
                                },
                                manifest: state,
                            },
                        );
                    }
                }
                return (
                    Self {
                        entries,
                        blocked: discovery.unresolvable_ids.clone(),
                        traversal_failure: discovery.traversal_failures.first().cloned(),
                        references: Self::references(&discovery),
                        fingerprint: (before == after).then_some(after),
                    },
                    discovery,
                );
            }
        }
        unreachable!("discovery loop returns on its final attempt")
    }

    pub(crate) fn source(&self, id: &str) -> genaimage_core::Result<Option<ThumbnailSource>> {
        if self.fingerprint.is_none() {
            return Err(HarnessError::Workflow(
                "Workflow files changed during discovery; retry when the catalogue is stable."
                    .into(),
            ));
        }
        if let Some(failure) = &self.traversal_failure {
            return Err(failure.for_id(id));
        }
        if self.blocked.contains(id) {
            return Err(HarnessError::Workflow(format!("workflow id '{id}' is ambiguous or claimed by an invalid manifest; inspect the catalogue diagnostics and resolve duplicates")));
        }
        let Some(cached) = self.entries.get(id) else {
            return Ok(None);
        };
        if let Ok(current) = entry(&cached.source.manifest_path) {
            if cached.manifest.as_ref() == Some(&current) {
                return Ok(Some(cached.source.clone()));
            }
            if current.target.is_none() {
                return Ok(None);
            }
        }
        let Ok(workflow) = LoadedWorkflow::load(&cached.source.manifest_path) else {
            return Ok(None);
        };
        if workflow.manifest.id != id {
            return Ok(None);
        }
        Ok(Some(ThumbnailSource {
            manifest_path: workflow.manifest_path,
            thumbnail: workflow.manifest.thumbnail,
        }))
    }
}

struct Snapshot {
    harness: Arc<Harness>,
    checked: Instant,
    index: Arc<WorkflowIndex>,
}

pub(crate) struct WorkflowIndexCache {
    snapshot: RwLock<Option<Snapshot>>,
    refresh: Arc<Mutex<()>>,
    probe: ScanProbe,
    interval: Duration,
}

impl Default for WorkflowIndexCache {
    fn default() -> Self {
        Self {
            snapshot: RwLock::new(None),
            refresh: Arc::default(),
            probe: ScanProbe::default(),
            interval: REVALIDATE_INTERVAL,
        }
    }
}

impl WorkflowIndexCache {
    fn fresh(&self, harness: &Arc<Harness>) -> Option<Arc<WorkflowIndex>> {
        let state = self.snapshot.read().expect("workflow index lock poisoned");
        let state = state.as_ref().filter(|state| {
            Arc::ptr_eq(&state.harness, harness) && state.checked.elapsed() < self.interval
        })?;
        Some(state.index.clone())
    }

    pub(crate) async fn get(
        self: &Arc<Self>,
        harness: Arc<Harness>,
    ) -> genaimage_core::Result<Arc<WorkflowIndex>> {
        if let Some(index) = self.fresh(&harness) {
            return Ok(index);
        }
        let guard = self.refresh.clone().lock_owned().await;
        if let Some(index) = self.fresh(&harness) {
            return Ok(index);
        }
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let old = cache
                .snapshot
                .read()
                .expect("workflow index lock poisoned")
                .as_ref()
                .filter(|state| Arc::ptr_eq(&state.harness, &harness))
                .map(|state| state.index.clone());
            let index = match old {
                Some(old)
                    if old.fingerprint.as_ref().is_some_and(|fingerprint_before| {
                        fingerprint(&harness.config().workflows, &old.references, &cache.probe)
                            == *fingerprint_before
                    }) =>
                {
                    old
                }
                _ => Arc::new(WorkflowIndex::rebuild(harness.clone(), &cache.probe).0),
            };
            cache.publish(harness, index.clone());
            index
        })
        .await
        .map_err(|error| HarnessError::Workflow(format!("Workflow index task failed: {error}")))
    }

    pub(crate) async fn list(
        self: &Arc<Self>,
        harness: Arc<Harness>,
    ) -> genaimage_core::Result<WorkflowDiscovery> {
        let guard = self.refresh.clone().lock_owned().await;
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let (index, discovery) = WorkflowIndex::rebuild(harness.clone(), &cache.probe);
            cache.publish(harness, Arc::new(index));
            discovery
        })
        .await
        .map_err(|error| HarnessError::Workflow(format!("Workflow index task failed: {error}")))
    }

    fn publish(&self, harness: Arc<Harness>, index: Arc<WorkflowIndex>) {
        *self.snapshot.write().expect("workflow index lock poisoned") = Some(Snapshot {
            harness,
            checked: Instant::now(),
            index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::future::join_all;
    use genaimage_comfyui::ComfyUiClient;
    use genaimage_core::{config::ServerConfig, HarnessConfig};
    use genaimage_test_support::{install_contract_workflow, ApiFixture};
    use serde_json::{json, Value};
    use std::sync::atomic::Ordering;
    use tower::ServiceExt;

    #[test]
    fn reference_errors_are_fingerprinted_and_can_recover() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let parent = root.join("not-a-directory");
        fs::write(&parent, b"file").unwrap();
        let graph = parent.join("graph.json");
        let references = BTreeSet::from([graph.clone()]);
        let probe = ScanProbe::default();
        let broken = fingerprint(root, &references, &probe);
        assert!(broken.references[&graph].is_err());
        assert_eq!(broken, fingerprint(root, &references, &probe));
        fs::remove_file(&parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(&graph, b"{}").unwrap();
        let repaired = fingerprint(root, &references, &probe);
        assert!(repaired.references[&graph].is_ok());
        assert_ne!(broken, repaired);
    }

    #[tokio::test]
    async fn final_attempt_change_forces_a_full_rebuild_on_revalidation() {
        let fixture = ApiFixture::start(5, ServerConfig::default().max_upload_bytes).await;
        let workspace = fixture.workspace.path();
        let config =
            HarnessConfig::load_with(workspace, Some(&fixture.config_path), workspace, |key| {
                (key == "HOME").then(|| workspace.display().to_string())
            })
            .unwrap();
        install_contract_workflow(&config.workflows);
        let graph_path = config.workflows.join("workflow.api.json");
        let graph = fs::read(&graph_path).unwrap();
        let mut later: Value = serde_json::from_slice(
            &fs::read(config.workflows.join("contract.genaimage.json")).unwrap(),
        )
        .unwrap();
        later["id"] = json!("later");
        let later_path = config.workflows.join("later.genaimage.json");
        let backend = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let harness = Arc::new(Harness::new_async(config, backend).await.unwrap());
        let cache = Arc::new(WorkflowIndexCache::default());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = count.clone();
        *cache.probe.after_discovery.lock().unwrap() = Some(Box::new(move || {
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 | 1 => {
                    let mut bytes = graph.clone();
                    bytes.extend(vec![b' '; calls.load(Ordering::SeqCst)]);
                    fs::write(&graph_path, bytes).unwrap();
                }
                2 => fs::write(&later_path, serde_json::to_vec(&later).unwrap()).unwrap(),
                _ => {}
            }
        }));
        let first = cache.list(harness.clone()).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(first.manifests.len(), 1);
        assert!(first
            .diagnostics
            .iter()
            .any(|item| item.error.to_string().contains("changed during discovery")));
        {
            let mut snapshot = cache.snapshot.write().unwrap();
            let snapshot = snapshot.as_mut().unwrap();
            assert!(snapshot.index.fingerprint.is_none());
            assert!(snapshot.index.source("contract-workflow").is_err());
            snapshot.checked = Instant::now() - Duration::from_secs(2);
        }
        let index = cache.get(harness).await.unwrap();
        assert!(index.fingerprint.is_some());
        assert!(index.source("later").unwrap().is_some());
    }

    #[tokio::test]
    async fn concurrent_thumbnail_requests_share_full_scans() {
        let fixture = ApiFixture::start(5, ServerConfig::default().max_upload_bytes).await;
        let workspace = fixture.workspace.path();
        let config =
            HarnessConfig::load_with(workspace, Some(&fixture.config_path), workspace, |key| {
                (key == "HOME").then(|| workspace.display().to_string())
            })
            .unwrap();
        install_contract_workflow(&config.workflows);
        let path = config.workflows.join("contract.genaimage.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["thumbnail"] = json!("image.png");
        fs::remove_file(&path).unwrap();
        for i in 0..100 {
            manifest["id"] = json!(format!("workflow-{i}"));
            fs::write(
                config
                    .workflows
                    .join(format!("workflow-{i}.genaimage.json")),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        }
        fs::write(config.workflows.join("image.png"), b"image").unwrap();
        let graph_path = config.workflows.join("workflow.api.json");
        let graph = fs::read(&graph_path).unwrap();
        let backend = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let harness = Arc::new(Harness::new_async(config, backend).await.unwrap());
        let mut state = crate::AppState::new(harness);
        Arc::get_mut(&mut state.workflow_index).unwrap().interval = Duration::from_secs(60);
        let port = fixture.listener.local_addr().unwrap().port();
        let app = crate::build_app(state.clone(), port);
        let requests = || {
            (0..100).map(|i| {
                let app = app.clone();
                async move {
                    app.oneshot(
                        Request::builder()
                            .uri(format!("/api/v1/workflows/workflow-{i}/thumbnail"))
                            .header("host", format!("127.0.0.1:{port}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
                }
            })
        };
        assert!(join_all(requests())
            .await
            .into_iter()
            .all(|status| status == StatusCode::OK));
        assert_eq!(state.workflow_index.probe.scans.load(Ordering::SeqCst), 4);
        let source = state
            .workflow_index
            .snapshot
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .index
            .entries["workflow-0"]
            .source
            .clone();
        let fallback = WorkflowIndex {
            entries: BTreeMap::from([(
                "workflow-0".into(),
                IndexedWorkflow {
                    source,
                    manifest: None,
                },
            )]),
            blocked: BTreeSet::new(),
            traversal_failure: None,
            references: BTreeSet::new(),
            fingerprint: Some(Fingerprint {
                entries: BTreeMap::new(),
                references: BTreeMap::new(),
                issues: Vec::new(),
            }),
        };
        assert!(fallback.source("workflow-0").unwrap().is_some());
        state
            .workflow_index
            .snapshot
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .checked = Instant::now() - Duration::from_secs(61);
        assert!(join_all(requests())
            .await
            .into_iter()
            .all(|status| status == StatusCode::OK));
        assert_eq!(state.workflow_index.probe.scans.load(Ordering::SeqCst), 5);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows")
                    .header("host", format!("127.0.0.1:{port}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.workflow_index.probe.scans.load(Ordering::SeqCst), 9);
        fs::write(&graph_path, b"{}").unwrap();
        state
            .workflow_index
            .snapshot
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .checked = Instant::now() - Duration::from_secs(61);
        let status = join_all(requests()).await;
        assert!(status
            .into_iter()
            .all(|status| status == StatusCode::INTERNAL_SERVER_ERROR));
        fs::write(&graph_path, graph).unwrap();
        state
            .workflow_index
            .snapshot
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .checked = Instant::now() - Duration::from_secs(61);
        assert!(join_all(requests())
            .await
            .into_iter()
            .all(|status| status == StatusCode::OK));
    }
}
