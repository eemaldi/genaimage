//! Explicit model bootstrap: list installed LLM models, pull one only on a user
//! action, and list ComfyUI checkpoints. Nothing here starts a download by
//! itself; callers must pass an explicit request.

use std::{
    collections::{BTreeMap, VecDeque},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{Harness, HarnessError, Result};

pub const MAX_MODEL_NAME_BYTES: usize = 255;
const MAX_NAME_SEGMENTS: usize = 3;
pub const CHECKPOINT_LOADER: &str = "CheckpointLoaderSimple";
const DEFAULT_MAX_CONCURRENT_PULLS: usize = 2;
const DEFAULT_RETAINED_PULLS: usize = 50;
const MAX_TRACKED_LAYERS: usize = 256;
const MAX_DIGEST_BYTES: usize = 128;

#[derive(Debug, Error, Eq, PartialEq)]
#[error("{0}")]
pub struct ModelNameError(String);

/// Accepts the Ollama grammar `[host/][namespace/]name[:tag]` conservatively:
/// at most three path segments of ASCII letters, digits, `.`, `_` and `-`, and
/// an optional tag after the last `:`. Nothing that could alter a URL or a
/// request body is accepted.
pub fn validate_model_name(name: &str) -> std::result::Result<(), ModelNameError> {
    let invalid = |reason: &str| ModelNameError(format!("Invalid model name: {reason}."));
    if name.is_empty() {
        return Err(invalid("it is empty"));
    }
    if name.len() > MAX_MODEL_NAME_BYTES {
        return Err(invalid(&format!(
            "it is longer than {MAX_MODEL_NAME_BYTES} bytes"
        )));
    }
    let (path, tag) = match name.rsplit_once(':') {
        Some((path, tag)) => (path, Some(tag)),
        None => (name, None),
    };
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() > MAX_NAME_SEGMENTS {
        return Err(invalid("it has too many '/' separated parts"));
    }
    let valid_part = |part: &str| {
        let mut bytes = part.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            && !part.contains("..")
    };
    if !segments.iter().all(|segment| valid_part(segment)) || tag.is_some_and(|t| !valid_part(t)) {
        return Err(invalid(
            "use letters, digits, '.', '_' and '-' in the form [namespace/]name[:tag]",
        ));
    }
    Ok(())
}

/// The tag-less form means `:latest`; used only to detect duplicate pulls.
fn canonical_name(name: &str) -> String {
    match name.rsplit_once(':') {
        Some(_) => name.to_owned(),
        None => format!("{name}:latest"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelSource {
    /// The Ollama native API (`/api/tags`) answered.
    Ollama,
    /// Only the OpenAI-compatible `/v1/models` listing is available.
    OpenAi,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledModel {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub digest: Option<String>,
    pub modified_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelListing {
    pub source: ModelSource,
    pub pull_supported: bool,
    /// Why a pull is unavailable; set when `pull_supported` is false.
    pub pull_unsupported_reason: Option<String>,
    pub models: Vec<InstalledModel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullProgress {
    pub status: String,
    pub digest: Option<String>,
    pub total_bytes: Option<u64>,
    pub completed_bytes: Option<u64>,
}

/// One item of a pull stream. A stream always ends with exactly one of
/// `Success` or `Failed`; a stream that ends without Ollama's success
/// terminator must be reported as `Failed`, never as success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullEvent {
    Progress(PullProgress),
    Success,
    Failed { message: String },
}

pub type PullStream = Pin<Box<dyn Stream<Item = PullEvent> + Send>>;

/// Port for installing and listing local LLM models.
#[async_trait]
pub trait LlmModelManager: Send + Sync {
    async fn list_installed(&self) -> Result<ModelListing>;

    /// Starts one explicitly requested pull. Returns `UnsupportedOperation`
    /// when the endpoint has no native pull API. Callers validate `name`.
    async fn pull(&self, name: &str) -> Result<PullStream>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl PullState {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullSnapshot {
    pub id: String,
    pub name: String,
    pub state: PullState,
    pub status: String,
    /// Layer announced by the most recent progress line.
    pub layer: Option<String>,
    /// Sum over the layers announced so far; it can grow as more layers start.
    pub total_bytes: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Increases with every update, so clients can order snapshots.
    pub sequence: u64,
}

#[derive(Debug, Error)]
pub enum PullStartError {
    #[error(transparent)]
    InvalidName(#[from] ModelNameError),
    #[error("a pull of '{}' is already in progress (pull {})", .0.name, .0.id)]
    InProgress(Box<PullSnapshot>),
    #[error("{max} pulls are already running; wait for one to finish or cancel it")]
    TooManyPulls { max: usize },
    #[error("the server is shutting down")]
    ShuttingDown,
    #[error(transparent)]
    Backend(#[from] HarnessError),
}

struct Entry {
    snapshot: PullSnapshot,
    canonical: String,
    layers: BTreeMap<String, (u64, u64)>,
    sender: watch::Sender<PullSnapshot>,
    cancel: CancellationToken,
}

impl Entry {
    fn publish(&mut self) {
        self.snapshot.sequence += 1;
        self.sender.send_replace(self.snapshot.clone());
    }

    fn finish(&mut self, state: PullState, status: &str, error: Option<String>) {
        self.snapshot.state = state;
        self.snapshot.status = status.to_owned();
        self.snapshot.error = error;
        self.snapshot.finished_at = Some(Utc::now());
        self.publish();
    }
}

/// Ends a pull that is still running when its owner goes away: the request
/// future was dropped before the pull task started, or the task panicked or
/// was aborted. A finished pull is left unchanged.
struct PullGuard {
    registry: PullRegistry,
    id: String,
    message: &'static str,
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        let message = self.message;
        self.registry.update(&self.id, |entry| {
            entry.finish(PullState::Failed, "failed", Some(message.to_owned()));
        });
    }
}

#[derive(Default)]
struct Inner {
    entries: BTreeMap<String, Entry>,
    order: VecDeque<String>,
}

/// In-memory registry of explicit pulls. Pulls are server-side jobs that clients
/// can observe and re-attach to; they are not persisted, so after a restart a
/// pull id is unknown. An unknown id never means success.
#[derive(Clone)]
pub struct PullRegistry {
    inner: Arc<Mutex<Inner>>,
    shutdown: CancellationToken,
    max_concurrent: usize,
    retained: usize,
}

impl PullRegistry {
    pub fn new(shutdown: CancellationToken) -> Self {
        Self {
            inner: Arc::default(),
            shutdown,
            max_concurrent: DEFAULT_MAX_CONCURRENT_PULLS,
            retained: DEFAULT_RETAINED_PULLS,
        }
    }

    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = max_concurrent.max(1);
        self
    }

    /// Starts a pull after an explicit request. `on_success` runs after the
    /// pull has fully succeeded. Returns once the endpoint has accepted the
    /// request; progress continues in the background.
    pub async fn start(
        &self,
        manager: Arc<dyn LlmModelManager>,
        name: &str,
        on_success: impl FnOnce() + Send + 'static,
    ) -> std::result::Result<PullSnapshot, PullStartError> {
        validate_model_name(name)?;
        if self.shutdown.is_cancelled() {
            return Err(PullStartError::ShuttingDown);
        }
        let (id, cancel) = self.reserve(name)?;
        let mut guard = PullGuard {
            registry: self.clone(),
            id: id.clone(),
            message: "The request that started this pull was interrupted.",
        };
        let stream = match manager.pull(name).await {
            Ok(stream) => stream,
            Err(error) => {
                self.release(&id);
                return Err(error.into());
            }
        };
        guard.message = "The pull task ended unexpectedly.";
        let snapshot = self.get(&id).expect("a reserved pull exists");
        let registry = self.clone();
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            registry
                .drive(guard, stream, cancel, shutdown, on_success)
                .await;
        });
        Ok(snapshot)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn reserve(
        &self,
        name: &str,
    ) -> std::result::Result<(String, CancellationToken), PullStartError> {
        let canonical = canonical_name(name);
        let mut inner = self.lock();
        let running = inner
            .entries
            .values()
            .filter(|entry| !entry.snapshot.state.is_terminal());
        if let Some(existing) = running.clone().find(|entry| entry.canonical == canonical) {
            return Err(PullStartError::InProgress(Box::new(
                existing.snapshot.clone(),
            )));
        }
        if running.count() >= self.max_concurrent {
            return Err(PullStartError::TooManyPulls {
                max: self.max_concurrent,
            });
        }
        let id = Uuid::new_v4().to_string();
        let snapshot = PullSnapshot {
            id: id.clone(),
            name: name.to_owned(),
            state: PullState::Running,
            status: "starting".to_owned(),
            layer: None,
            total_bytes: None,
            completed_bytes: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            sequence: 0,
        };
        let cancel = CancellationToken::new();
        let (sender, _) = watch::channel(snapshot.clone());
        inner.entries.insert(
            id.clone(),
            Entry {
                snapshot,
                canonical,
                layers: BTreeMap::new(),
                sender,
                cancel: cancel.clone(),
            },
        );
        inner.order.push_back(id.clone());
        let retained = self.retained;
        while inner.order.len() > retained {
            let oldest_finished = inner.order.iter().position(|candidate| {
                !inner
                    .entries
                    .get(candidate)
                    .is_some_and(|entry| !entry.snapshot.state.is_terminal())
            });
            let Some(position) = oldest_finished else {
                break;
            };
            let evicted = inner.order.remove(position).expect("position in range");
            inner.entries.remove(&evicted);
        }
        Ok((id, cancel))
    }

    fn release(&self, id: &str) {
        let mut inner = self.lock();
        inner.entries.remove(id);
        inner.order.retain(|candidate| candidate != id);
    }

    async fn drive(
        &self,
        guard: PullGuard,
        mut stream: PullStream,
        cancel: CancellationToken,
        shutdown: CancellationToken,
        on_success: impl FnOnce(),
    ) {
        let id = guard.id.as_str();
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = shutdown.cancelled() => {
                    self.update(id, |entry| entry.finish(
                        PullState::Cancelled,
                        "cancelled",
                        Some("The server stopped before the pull finished.".to_owned()),
                    ));
                    return;
                }
                event = stream.next() => event,
            };
            match event {
                Some(PullEvent::Progress(progress)) => {
                    self.update(id, |entry| {
                        entry.snapshot.status = progress.status;
                        if let (Some(digest), Some(total)) = (progress.digest, progress.total_bytes)
                        {
                            let known = entry.layers.contains_key(&digest);
                            if digest.len() <= MAX_DIGEST_BYTES
                                && (known || entry.layers.len() < MAX_TRACKED_LAYERS)
                            {
                                entry.layers.insert(
                                    digest.clone(),
                                    (total, progress.completed_bytes.unwrap_or(0)),
                                );
                                entry.snapshot.layer = Some(digest);
                                entry.snapshot.total_bytes = Some(
                                    entry
                                        .layers
                                        .values()
                                        .fold(0u64, |sum, layer| sum.saturating_add(layer.0)),
                                );
                                entry.snapshot.completed_bytes = Some(
                                    entry
                                        .layers
                                        .values()
                                        .fold(0u64, |sum, layer| sum.saturating_add(layer.1)),
                                );
                            }
                        }
                        entry.publish();
                    });
                }
                Some(PullEvent::Success) => {
                    let finished = self.update(id, |entry| {
                        entry.finish(PullState::Succeeded, "success", None);
                    });
                    if finished {
                        on_success();
                    }
                    return;
                }
                Some(PullEvent::Failed { message }) => {
                    self.update(id, |entry| {
                        entry.finish(PullState::Failed, "failed", Some(message));
                    });
                    return;
                }
                None => {
                    self.update(id, |entry| {
                        entry.finish(
                            PullState::Failed,
                            "failed",
                            Some("The pull stream ended without a result.".to_owned()),
                        );
                    });
                    return;
                }
            }
        }
    }

    /// Applies `change` unless the pull already reached a terminal state.
    fn update(&self, id: &str, change: impl FnOnce(&mut Entry)) -> bool {
        let mut inner = self.lock();
        match inner.entries.get_mut(id) {
            Some(entry) if !entry.snapshot.state.is_terminal() => {
                change(entry);
                true
            }
            _ => false,
        }
    }

    /// Newest first.
    pub fn list(&self) -> Vec<PullSnapshot> {
        let inner = self.lock();
        inner
            .order
            .iter()
            .rev()
            .filter_map(|id| inner.entries.get(id).map(|entry| entry.snapshot.clone()))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PullSnapshot> {
        let inner = self.lock();
        inner.entries.get(id).map(|entry| entry.snapshot.clone())
    }

    /// Subscribes to snapshots of one pull. The receiver holds the latest
    /// snapshot immediately, so a client can attach at any time.
    pub fn subscribe(&self, id: &str) -> Option<watch::Receiver<PullSnapshot>> {
        let inner = self.lock();
        inner.entries.get(id).map(|entry| entry.sender.subscribe())
    }

    /// Cancels a running pull and records `cancelled`. A finished pull is
    /// returned unchanged.
    pub fn cancel(&self, id: &str) -> Option<PullSnapshot> {
        let mut inner = self.lock();
        let entry = inner.entries.get_mut(id)?;
        if !entry.snapshot.state.is_terminal() {
            entry.cancel.cancel();
            entry.finish(PullState::Cancelled, "cancelled", None);
        }
        Some(entry.snapshot.clone())
    }
}

/// Reads `CheckpointLoaderSimple` `ckpt_name` options from ComfyUI.
///
/// A ComfyUI that cannot be reached, or that lacks the loader node, is an
/// error: an empty list would wrongly say that no checkpoints are installed.
pub async fn list_checkpoints(harness: &Harness) -> Result<Vec<String>> {
    let info = harness.backend().object_info().await?;
    checkpoint_names(&info)
}

pub fn checkpoint_names(object_info: &Value) -> Result<Vec<String>> {
    let missing = || {
        HarnessError::ComfyUi(format!(
            "ComfyUI does not list the {CHECKPOINT_LOADER} node or its ckpt_name options; \
             update ComfyUI or check that its core nodes loaded"
        ))
    };
    let field = object_info
        .pointer(&format!("/{CHECKPOINT_LOADER}/input/required/ckpt_name"))
        .and_then(Value::as_array)
        .ok_or_else(missing)?;
    let options = match field.first() {
        Some(Value::Array(options)) => options,
        Some(Value::String(_)) => field
            .get(1)
            .and_then(|config| config.get("options"))
            .and_then(Value::as_array)
            .ok_or_else(missing)?,
        _ => return Err(missing()),
    };
    let mut names = options
        .iter()
        .map(|option| option.as_str().map(str::to_owned).ok_or_else(missing))
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serde_json::json;

    #[test]
    fn model_names_follow_the_ollama_grammar() {
        for good in [
            "llama3",
            "llama3.2:3b",
            "library/llama3:latest",
            "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M",
            "qwen2.5vl:7b",
        ] {
            assert_eq!(validate_model_name(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            " llama3",
            "llama3 ",
            "llama 3",
            "a/b/c/d",
            "../etc/passwd",
            "llama3:",
            ":tag",
            "llama3:a:b",
            "llama3\n",
            "llama3\u{7f}",
            "http://evil/x",
            "a@sha256:abc",
            "a?x=1",
            "a%2f",
            "-leading",
            "über",
            &"a".repeat(MAX_MODEL_NAME_BYTES + 1),
        ] {
            assert!(validate_model_name(bad).is_err(), "{bad:?}");
        }
    }

    struct Hanging;

    #[async_trait]
    impl LlmModelManager for Hanging {
        async fn list_installed(&self) -> Result<ModelListing> {
            unimplemented!()
        }

        async fn pull(&self, _name: &str) -> Result<PullStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }

    #[tokio::test]
    async fn a_tagless_name_duplicates_its_latest_tag() {
        let registry = PullRegistry::new(CancellationToken::new());
        let first = registry
            .start(Arc::new(Hanging), "llama3", || {})
            .await
            .unwrap();
        let duplicate = registry
            .start(Arc::new(Hanging), "llama3:latest", || {})
            .await
            .unwrap_err();
        assert!(
            matches!(duplicate, PullStartError::InProgress(existing) if existing.id == first.id)
        );
        assert!(registry
            .start(Arc::new(Hanging), "llama3:8b", || {})
            .await
            .is_ok());
        assert!(matches!(
            registry.start(Arc::new(Hanging), "other", || {}).await,
            Err(PullStartError::TooManyPulls { max: 2 })
        ));
    }

    struct Stalled;

    #[async_trait]
    impl LlmModelManager for Stalled {
        async fn list_installed(&self) -> Result<ModelListing> {
            unimplemented!()
        }

        async fn pull(&self, _name: &str) -> Result<PullStream> {
            std::future::pending().await
        }
    }

    struct Panicking;

    #[async_trait]
    impl LlmModelManager for Panicking {
        async fn list_installed(&self) -> Result<ModelListing> {
            unimplemented!()
        }

        async fn pull(&self, _name: &str) -> Result<PullStream> {
            Ok(Box::pin(futures_util::stream::poll_fn(|_| {
                panic!("stream failure")
            })))
        }
    }

    async fn settled(registry: &PullRegistry, id: &str) -> PullSnapshot {
        for _ in 0..100 {
            let snapshot = registry.get(id).unwrap();
            if snapshot.state.is_terminal() {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("pull did not settle");
    }

    #[tokio::test]
    async fn a_dropped_start_request_frees_the_name_and_the_slot() {
        let registry = PullRegistry::new(CancellationToken::new());
        let abandoned = tokio::time::timeout(
            Duration::from_millis(50),
            registry.start(Arc::new(Stalled), "llama3", || {}),
        )
        .await;
        assert!(abandoned.is_err());
        let pulls = registry.list();
        assert_eq!(pulls.len(), 1);
        assert_eq!(pulls[0].state, PullState::Failed);
        assert!(pulls[0].error.as_deref().unwrap().contains("interrupted"));
        assert!(registry
            .start(Arc::new(Hanging), "llama3", || {})
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_panicking_pull_task_ends_failed_and_frees_the_slot() {
        let registry = PullRegistry::new(CancellationToken::new());
        let started = registry
            .start(Arc::new(Panicking), "llama3", || {})
            .await
            .unwrap();
        let snapshot = settled(&registry, &started.id).await;
        assert_eq!(snapshot.state, PullState::Failed);
        assert!(snapshot.error.as_deref().unwrap().contains("unexpectedly"));
        assert!(registry
            .start(Arc::new(Hanging), "llama3", || {})
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn concurrent_aliases_start_exactly_one_pull() {
        let registry = PullRegistry::new(CancellationToken::new());
        let (first, second) = tokio::join!(
            registry.start(Arc::new(Hanging), "llama3", || {}),
            registry.start(Arc::new(Hanging), "llama3:latest", || {}),
        );
        assert_eq!(
            [first.is_ok(), second.is_ok()]
                .into_iter()
                .filter(|accepted| *accepted)
                .count(),
            1
        );
        assert!(matches!(
            first.err().or(second.err()),
            Some(PullStartError::InProgress(_))
        ));
    }

    #[tokio::test]
    async fn retention_never_evicts_a_running_pull() {
        let mut registry = PullRegistry::new(CancellationToken::new());
        registry.retained = 2;
        let running = registry
            .start(Arc::new(Hanging), "keep", || {})
            .await
            .unwrap();
        for index in 0..5 {
            let pull = registry
                .start(Arc::new(Hanging), &format!("other{index}"), || {})
                .await
                .unwrap();
            registry.cancel(&pull.id).unwrap();
        }
        assert_eq!(registry.get(&running.id).unwrap().state, PullState::Running);
        assert_eq!(registry.list().len(), 2);
    }

    struct Flood;

    #[async_trait]
    impl LlmModelManager for Flood {
        async fn list_installed(&self) -> Result<ModelListing> {
            unimplemented!()
        }

        async fn pull(&self, _name: &str) -> Result<PullStream> {
            let layer = |digest: String| {
                PullEvent::Progress(PullProgress {
                    status: "pulling".to_owned(),
                    digest: Some(digest),
                    total_bytes: Some(u64::MAX),
                    completed_bytes: Some(u64::MAX),
                })
            };
            let mut events = (0..MAX_TRACKED_LAYERS + 10)
                .map(|index| layer(format!("sha256:{index}")))
                .collect::<Vec<_>>();
            events.push(layer("x".repeat(MAX_DIGEST_BYTES + 1)));
            events.push(PullEvent::Success);
            Ok(Box::pin(futures_util::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn untrusted_progress_saturates_and_caps_tracked_layers() {
        let registry = PullRegistry::new(CancellationToken::new());
        let started = registry
            .start(Arc::new(Flood), "llama3", || {})
            .await
            .unwrap();
        let snapshot = settled(&registry, &started.id).await;
        assert_eq!(snapshot.state, PullState::Succeeded);
        assert_eq!(snapshot.total_bytes, Some(u64::MAX));
        assert_eq!(snapshot.completed_bytes, Some(u64::MAX));
        assert_eq!(snapshot.layer.as_deref(), Some("sha256:255"));
    }

    #[test]
    fn checkpoints_come_from_either_object_info_shape() {
        let legacy = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["b.safetensors", "a.ckpt"]]}}}});
        assert_eq!(
            checkpoint_names(&legacy).unwrap(),
            ["a.ckpt", "b.safetensors"]
        );
        let combo = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": ["COMBO", {"options": ["x.safetensors"]}]}}}});
        assert_eq!(checkpoint_names(&combo).unwrap(), ["x.safetensors"]);
        let empty = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [[]]}}}});
        assert!(checkpoint_names(&empty).unwrap().is_empty());
        assert!(checkpoint_names(&json!({"CheckpointLoaderSimple": {}})).is_err());
        assert!(checkpoint_names(&json!({})).is_err());
    }
}
