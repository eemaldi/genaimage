use std::{
    collections::BTreeMap,
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use genaimage_core::{orchestrator::WorkflowDiscovery, Harness, HarnessError, LoadedWorkflow};

#[derive(Debug, Eq, PartialEq)]
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

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    link: Option<Stamp>,
    target: Option<Stamp>,
}

type Fingerprint = BTreeMap<PathBuf, Entry>;

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

fn record(path: &Path, entries: &mut Fingerprint) -> io::Result<bool> {
    let link = metadata(path, false)?;
    let target = metadata(path, true)?;
    let directory = target.as_ref().is_some_and(Metadata::is_dir);
    entries.insert(
        path.to_path_buf(),
        Entry {
            link: link.as_ref().map(Stamp::new).transpose()?,
            target: target.as_ref().map(Stamp::new).transpose()?,
        },
    );
    Ok(directory)
}

fn record_tree(path: &Path, entries: &mut Fingerprint) -> io::Result<()> {
    if record(path, entries)? {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
                record_tree(&path, entries)?;
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".genaimage.json"))
            {
                record(&path, entries)?;
            }
        }
    }
    Ok(())
}

fn fingerprint(root: &Path) -> io::Result<Fingerprint> {
    let mut entries = BTreeMap::new();
    record_tree(root, &mut entries)?;
    Ok(entries)
}

pub(crate) struct WorkflowIndex {
    harness: Arc<Harness>,
    paths: BTreeMap<String, PathBuf>,
    fingerprint: Fingerprint,
}

impl WorkflowIndex {
    pub(crate) fn rebuild(
        harness: Arc<Harness>,
    ) -> genaimage_core::Result<(Self, WorkflowDiscovery)> {
        for _ in 0..3 {
            let before = fingerprint(&harness.config().workflows)?;
            let discovery = harness.discover_workflows()?;
            let after = fingerprint(&harness.config().workflows)?;
            if before == after {
                return Ok((
                    Self {
                        harness,
                        paths: discovery.indexable_manifest_paths.clone(),
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

    pub(crate) fn load(
        index: &mut Option<Self>,
        harness: Arc<Harness>,
        id: &str,
    ) -> genaimage_core::Result<Option<LoadedWorkflow>> {
        for _ in 0..3 {
            let fresh = if let Some(cached) = index
                .as_ref()
                .filter(|cached| Arc::ptr_eq(&cached.harness, &harness))
            {
                fingerprint(&harness.config().workflows)? == cached.fingerprint
            } else {
                false
            };
            if !fresh {
                *index = None;
                *index = Some(Self::rebuild(harness.clone())?.0);
            }
            let cached = index.as_ref().expect("workflow index rebuilt");
            let workflow = cached
                .paths
                .get(id)
                .and_then(|path| LoadedWorkflow::load(path).ok())
                .filter(|workflow| workflow.manifest.id == id);
            let result = match workflow {
                Some(workflow) => Ok(Some(workflow)),
                None => harness.load_workflow_by_id(id),
            };
            if fingerprint(&harness.config().workflows)? == cached.fingerprint {
                return result;
            }
            *index = None;
        }
        Err(HarnessError::Workflow(
            "Workflow files changed during lookup; try again when they are stable.".into(),
        ))
    }
}
