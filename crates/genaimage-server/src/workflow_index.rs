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
#[derive(Debug, Eq, PartialEq)]
struct ReferenceError {
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
}

#[derive(Debug, Eq, PartialEq)]
struct Fingerprint {
    entries: BTreeMap<PathBuf, Entry>,
    references: BTreeMap<PathBuf, Result<Entry, ReferenceError>>,
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

fn record_tree(path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) -> io::Result<()> {
    let state = entry(path)?;
    let directory = state.target.as_ref().is_some_and(|stamp| stamp.directory);
    entries.insert(path.to_path_buf(), state);
    if directory {
        for item in fs::read_dir(path)? {
            let item = item?;
            let path = item.path();
            let kind = item.file_type()?;
            if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
                record_tree(&path, entries)?;
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".genaimage.json"))
            {
                entries.insert(path.clone(), entry(&path)?);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct ScanProbe {
    #[cfg(test)]
    scans: Arc<std::sync::atomic::AtomicUsize>,
}

fn fingerprint(
    root: &Path,
    references: &BTreeSet<PathBuf>,
    probe: &ScanProbe,
) -> io::Result<Fingerprint> {
    #[cfg(test)]
    probe
        .scans
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let _ = probe;
    let mut entries = BTreeMap::new();
    record_tree(root, &mut entries)?;
    let references = references
        .iter()
        .map(|path| {
            let state = entry(path).map_err(|error| ReferenceError {
                kind: error.kind(),
                raw_os_error: error.raw_os_error(),
            });
            (path.clone(), state)
        })
        .collect();
    Ok(Fingerprint {
        entries,
        references,
    })
}

fn discover(harness: &Harness, probe: &ScanProbe) -> genaimage_core::Result<WorkflowDiscovery> {
    #[cfg(test)]
    probe
        .scans
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let _ = probe;
    harness.discover_workflows()
}

#[derive(Clone)]
pub(crate) struct ThumbnailSource {
    pub(crate) manifest_path: PathBuf,
    pub(crate) thumbnail: Option<String>,
}

struct IndexedWorkflow {
    source: ThumbnailSource,
    manifest: Entry,
}

pub(crate) struct WorkflowIndex {
    entries: BTreeMap<String, IndexedWorkflow>,
    blocked: BTreeSet<String>,
    references: BTreeSet<PathBuf>,
    fingerprint: Fingerprint,
}

impl WorkflowIndex {
    fn rebuild(
        harness: Arc<Harness>,
        probe: &ScanProbe,
    ) -> genaimage_core::Result<(Self, WorkflowDiscovery)> {
        for _ in 0..3 {
            let first = discover(&harness, probe)?;
            let before = fingerprint(
                &harness.config().workflows,
                &first.referenced_workflow_paths,
                probe,
            )?;
            let discovery = discover(&harness, probe)?;
            let after = fingerprint(
                &harness.config().workflows,
                &discovery.referenced_workflow_paths,
                probe,
            )?;
            if before == after {
                let mut entries = BTreeMap::new();
                for manifest in &discovery.manifests {
                    if let Some(path) = discovery.indexable_manifest_paths.get(&manifest.id) {
                        let state = after.entries.get(path).ok_or_else(|| {
                            HarnessError::Workflow(
                                "Workflow disappeared during discovery; try again.".into(),
                            )
                        })?;
                        entries.insert(
                            manifest.id.clone(),
                            IndexedWorkflow {
                                source: ThumbnailSource {
                                    manifest_path: path.clone(),
                                    thumbnail: manifest.thumbnail.clone(),
                                },
                                manifest: state.clone(),
                            },
                        );
                    }
                }
                return Ok((
                    Self {
                        entries,
                        blocked: discovery.unresolvable_ids.clone(),
                        references: discovery.referenced_workflow_paths.clone(),
                        fingerprint: after,
                    },
                    discovery,
                ));
            }
        }
        Err(HarnessError::Workflow(
            "Workflow files changed during discovery; try again when they are stable.".into(),
        ))
    }

    pub(crate) fn source(&self, id: &str) -> genaimage_core::Result<Option<ThumbnailSource>> {
        if self.blocked.contains(id) {
            return Err(HarnessError::Workflow(format!("workflow id '{id}' is ambiguous or claimed by an invalid manifest; inspect the catalogue diagnostics and resolve duplicates")));
        }
        let Some(cached) = self.entries.get(id) else {
            return Ok(None);
        };
        let current = entry(&cached.source.manifest_path)?;
        if current == cached.manifest {
            return Ok(Some(cached.source.clone()));
        }
        if current.target.is_none() {
            return Ok(None);
        }
        let workflow = LoadedWorkflow::load(&cached.source.manifest_path)?;
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
    result: Result<Arc<WorkflowIndex>, String>,
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
    fn fresh(&self, harness: &Arc<Harness>) -> Option<genaimage_core::Result<Arc<WorkflowIndex>>> {
        let state = self.snapshot.read().expect("workflow index lock poisoned");
        let state = state.as_ref().filter(|state| {
            Arc::ptr_eq(&state.harness, harness) && state.checked.elapsed() < self.interval
        })?;
        Some(state.result.clone().map_err(HarnessError::Workflow))
    }

    pub(crate) async fn get(
        self: &Arc<Self>,
        harness: Arc<Harness>,
    ) -> genaimage_core::Result<Arc<WorkflowIndex>> {
        if let Some(result) = self.fresh(&harness) {
            return result;
        }
        let guard = self.refresh.clone().lock_owned().await;
        if let Some(result) = self.fresh(&harness) {
            return result;
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
                .and_then(|state| state.result.as_ref().ok())
                .cloned();
            let result = (|| {
                if let Some(old) = old {
                    if fingerprint(&harness.config().workflows, &old.references, &cache.probe)?
                        == old.fingerprint
                    {
                        return Ok(old);
                    }
                }
                Ok(Arc::new(
                    WorkflowIndex::rebuild(harness.clone(), &cache.probe)?.0,
                ))
            })();
            cache.publish(harness, &result);
            result
        })
        .await
        .map_err(|error| HarnessError::Workflow(format!("Workflow index task failed: {error}")))?
    }

    pub(crate) async fn list(
        self: &Arc<Self>,
        harness: Arc<Harness>,
    ) -> genaimage_core::Result<WorkflowDiscovery> {
        let guard = self.refresh.clone().lock_owned().await;
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let result = WorkflowIndex::rebuild(harness.clone(), &cache.probe)
                .map(|(index, discovery)| (Arc::new(index), discovery));
            let published = match &result {
                Ok((index, _)) => Ok(index.clone()),
                Err(error) => Err(HarnessError::Workflow(error.to_string())),
            };
            cache.publish(harness, &published);
            result.map(|(_, discovery)| discovery)
        })
        .await
        .map_err(|error| HarnessError::Workflow(format!("Workflow index task failed: {error}")))?
    }

    fn publish(&self, harness: Arc<Harness>, result: &genaimage_core::Result<Arc<WorkflowIndex>>) {
        *self.snapshot.write().expect("workflow index lock poisoned") = Some(Snapshot {
            harness,
            checked: Instant::now(),
            result: result.as_ref().map(Arc::clone).map_err(ToString::to_string),
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
        let broken = fingerprint(root, &references, &probe).unwrap();
        assert!(broken.references[&graph].is_err());
        assert_eq!(broken, fingerprint(root, &references, &probe).unwrap());
        fs::remove_file(&parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(&graph, b"{}").unwrap();
        let repaired = fingerprint(root, &references, &probe).unwrap();
        assert!(repaired.references[&graph].is_ok());
        assert_ne!(broken, repaired);
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
