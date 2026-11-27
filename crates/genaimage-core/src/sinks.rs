//! Event sink runtime: a bounded queue and delivery loop per sink, delivery
//! status for diagnostics, the append-only JSONL sink and signing helpers.
//!
//! The bus never waits for a sink. Each sink owns a forwarding task that moves
//! events into a bounded queue and a worker that delivers them in order. When
//! the queue is full the newest event is dropped and counted, and the next
//! queued notification is a gap marker. A bus lag re-subscribes at the live
//! tail instead of ending the sink; the gap is counted and marked, not hidden.

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom, Write},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{sync::Notify, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    Event, EventBus, EventEnvelope, EventSink, HarnessError, ReceiveError, ResolvedConfig, Result,
    ResyncReason, SinkNotification,
};

pub const DEFAULT_QUEUE_CAPACITY: usize = 256;
pub const DEFAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(3);
/// Longest line a sink writes, including the newline.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// Consecutive failed deliveries after which a sink is reported as failing.
pub const FAILING_THRESHOLD: u64 = 3;
const MAX_ERROR_CHARS: usize = 240;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SinkState {
    Ok,
    Degraded,
    Failing,
}

impl SinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Failing => "failing",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SinkStatusSnapshot {
    pub name: String,
    pub kind: String,
    pub state: SinkState,
    pub delivered: u64,
    pub failed: u64,
    pub dropped: u64,
    pub dead_lettered: u64,
    pub resyncs: u64,
    pub retries: u64,
    pub consecutive_failures: u64,
    /// Redacted and bounded.
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_resync_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct Latest {
    consecutive_failures: u64,
    stopped: bool,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_resync_at: Option<DateTime<Utc>>,
}

/// Shared delivery counters for one sink. Counters are totals since start.
pub struct SinkStatus {
    name: String,
    kind: &'static str,
    delivered: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    dead_lettered: AtomicU64,
    resyncs: AtomicU64,
    retries: AtomicU64,
    latest: Mutex<Latest>,
}

impl std::fmt::Debug for SinkStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SinkStatus")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl SinkStatus {
    pub fn new(name: &str, kind: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            kind,
            delivered: AtomicU64::default(),
            failed: AtomicU64::default(),
            dropped: AtomicU64::default(),
            dead_lettered: AtomicU64::default(),
            resyncs: AtomicU64::default(),
            retries: AtomicU64::default(),
            latest: Mutex::default(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn latest(&self) -> std::sync::MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn record_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        let mut latest = self.latest();
        latest.consecutive_failures = 0;
        latest.last_success_at = Some(Utc::now());
    }

    /// An event the sink could not deliver. `error` must already be free of secrets.
    pub fn record_failure(&self, error: &str) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        let mut latest = self.latest();
        latest.consecutive_failures += 1;
        latest.last_error = Some(bound(error));
        latest.last_error_at = Some(Utc::now());
    }

    /// A failed attempt that will be retried.
    pub fn record_retry(&self, error: &str) {
        self.retries.fetch_add(1, Ordering::Relaxed);
        let mut latest = self.latest();
        latest.last_error = Some(bound(error));
        latest.last_error_at = Some(Utc::now());
    }

    pub fn record_dropped(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_dead_letter(&self) {
        self.dead_lettered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_resync(&self) {
        self.resyncs.fetch_add(1, Ordering::Relaxed);
        self.latest().last_resync_at = Some(Utc::now());
    }

    /// The sink's task ended unexpectedly; it reports failing until restart.
    pub fn record_stopped(&self, error: &str) {
        let mut latest = self.latest();
        latest.stopped = true;
        latest.last_error = Some(bound(error));
        latest.last_error_at = Some(Utc::now());
    }

    pub fn snapshot(&self) -> SinkStatusSnapshot {
        let latest = self.latest();
        let counts = [
            &self.delivered,
            &self.failed,
            &self.dropped,
            &self.dead_lettered,
            &self.resyncs,
            &self.retries,
        ]
        .map(|counter| counter.load(Ordering::Relaxed));
        let [delivered, failed, dropped, dead_lettered, resyncs, retries] = counts;
        let state = if latest.stopped || latest.consecutive_failures >= FAILING_THRESHOLD {
            SinkState::Failing
        } else if latest.consecutive_failures > 0 || failed + dropped + dead_lettered + resyncs > 0
        {
            SinkState::Degraded
        } else {
            SinkState::Ok
        };
        SinkStatusSnapshot {
            name: self.name.clone(),
            kind: self.kind.to_owned(),
            state,
            delivered,
            failed,
            dropped,
            dead_lettered,
            resyncs,
            retries,
            consecutive_failures: latest.consecutive_failures,
            last_error: latest.last_error.clone(),
            last_error_at: latest.last_error_at,
            last_success_at: latest.last_success_at,
            last_resync_at: latest.last_resync_at,
        }
    }
}

/// The statuses of every running sink on a bus. Diagnostics read the same
/// values the delivery loops write.
#[derive(Clone, Default)]
pub struct SinkStatuses(Arc<RwLock<Vec<Arc<SinkStatus>>>>);

impl SinkStatuses {
    pub fn register(&self, status: Arc<SinkStatus>) {
        self.0
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(status);
    }

    pub fn snapshots(&self) -> Vec<SinkStatusSnapshot> {
        let mut snapshots: Vec<_> = self
            .0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|status| status.snapshot())
            .collect();
        snapshots.sort_by(|a, b| a.name.cmp(&b.name));
        snapshots
    }
}

/// Collapse a message to a bounded single line and remove the secret.
pub fn redact_and_bound(text: &str, secret: Option<&str>) -> String {
    match secret.filter(|secret| !secret.is_empty()) {
        Some(secret) => bound(&text.replace(secret, "<redacted>")),
        None => bound(text),
    }
}

fn bound(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_ERROR_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(MAX_ERROR_CHARS).collect();
    cut.push('…');
    cut
}

/// HMAC-SHA256 as lower-case hexadecimal (RFC 2104).
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0_u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|k| k ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    hex::encode(
        Sha256::new()
            .chain_update(pad(0x5c))
            .chain_update(inner)
            .finalize(),
    )
}

pub fn sink_dir(config: &ResolvedConfig) -> PathBuf {
    config.data_dir().join("sinks")
}

pub fn jsonl_path(config: &ResolvedConfig, relative: &str) -> PathBuf {
    sink_dir(config).join(relative)
}

pub fn dead_letter_path(config: &ResolvedConfig, sink_name: &str) -> PathBuf {
    sink_dir(config).join(format!("{sink_name}{DEAD_LETTER_SUFFIX}"))
}

pub const DEAD_LETTER_SUFFIX: &str = ".dead-letter.jsonl";

/// The explicit record of a gap, written by sinks that keep a log.
pub fn gap_marker(reason: &ResyncReason, at: DateTime<Utc>) -> serde_json::Value {
    let (name, skipped) = match reason {
        ResyncReason::Lagged { skipped } => ("lagged", Some(*skipped)),
        ResyncReason::Invalidated { .. } => ("invalidated", None),
        ResyncReason::CursorOutOfRange { .. } => ("cursor_out_of_range", None),
        ResyncReason::BusChanged { .. } => ("bus_changed", None),
    };
    json!({"type": "sink_gap", "reason": name, "skipped": skipped, "at": at})
}

/// Append one line with a single write, then fsync it. The file is opened per
/// write so an external rotation takes effect on the next line. A torn last
/// line from a crash is terminated first so the new line stays parseable.
pub async fn append_line(path: &Path, line: String) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || append_line_blocking(&path, &line))
        .await
        .map_err(io::Error::other)?
}

fn append_line_blocking(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let mut data = Vec::with_capacity(line.len() + 2);
    if file.metadata()?.len() > 0 {
        let mut reader = std::fs::File::open(path)?;
        reader.seek(SeekFrom::End(-1))?;
        let mut last = [0_u8; 1];
        reader.read_exact(&mut last)?;
        if last[0] != b'\n' {
            data.push(b'\n');
        }
    }
    data.extend_from_slice(line.as_bytes());
    data.push(b'\n');
    file.write_all(&data)?;
    file.sync_data()
}

/// Appends one JSON line per event, and one explicit marker per gap.
pub struct JsonlSink {
    path: PathBuf,
}

impl JsonlSink {
    pub fn new(path: PathBuf) -> Result<Self> {
        if path.is_dir() {
            return Err(HarnessError::Configuration(format!(
                "JSONL sink path '{}' is a directory",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                HarnessError::Configuration(format!(
                    "cannot create JSONL sink directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        Ok(Self { path })
    }
}

#[async_trait]
impl EventSink for JsonlSink {
    async fn handle(&self, notification: &SinkNotification) -> Result<()> {
        let line = match notification {
            SinkNotification::Event(envelope) => {
                let line = serde_json::to_string(envelope)
                    .map_err(|error| HarnessError::Configuration(error.to_string()))?;
                if line.len() >= MAX_LINE_BYTES {
                    let marker = json!({
                        "type": "sink_oversize",
                        "bus_id": envelope.bus_id,
                        "id": envelope.id,
                        "event_type": envelope.event.kind(),
                        "bytes": line.len(),
                        "at": Utc::now(),
                    });
                    append_line(&self.path, marker.to_string()).await?;
                    return Err(HarnessError::Configuration(format!(
                        "event {} is {} bytes, over the {MAX_LINE_BYTES} byte line limit",
                        envelope.id,
                        line.len()
                    )));
                }
                line
            }
            SinkNotification::ResyncRequired(reason) => gap_marker(reason, Utc::now()).to_string(),
        };
        Ok(append_line(&self.path, line).await?)
    }
}

struct Queue {
    state: Mutex<QueueState>,
    notify: Notify,
    capacity: usize,
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<SinkNotification>,
    pending_gap: u64,
    closed: bool,
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop-newest: a full queue rejects the event and a marker precedes the
    /// next accepted one.
    fn push_event(&self, envelope: EventEnvelope, status: &SinkStatus) {
        let mut state = self.lock();
        let needed = if state.pending_gap > 0 { 2 } else { 1 };
        if state.items.len() + needed > self.capacity {
            state.pending_gap += 1;
            status.record_dropped(1);
            return;
        }
        if state.pending_gap > 0 {
            let skipped = std::mem::take(&mut state.pending_gap);
            state
                .items
                .push_back(SinkNotification::ResyncRequired(ResyncReason::Lagged {
                    skipped,
                }));
        }
        state
            .items
            .push_back(SinkNotification::Event(Box::new(envelope)));
        drop(state);
        self.notify.notify_one();
    }

    fn push_gap(&self, reason: ResyncReason) {
        let mut state = self.lock();
        if state.items.len() < self.capacity {
            state
                .items
                .push_back(SinkNotification::ResyncRequired(reason));
            drop(state);
            self.notify.notify_one();
        } else {
            state.pending_gap += match reason {
                ResyncReason::Lagged { skipped } => skipped.max(1),
                _ => 1,
            };
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.notify.notify_one();
    }

    async fn pop(&self) -> Option<SinkNotification> {
        loop {
            {
                let mut state = self.lock();
                if let Some(item) = state.items.pop_front() {
                    return Some(item);
                }
                if state.closed {
                    return None;
                }
            }
            self.notify.notified().await;
        }
    }

    /// Close the queue and return how many events were still waiting.
    fn discard(&self) -> u64 {
        let mut state = self.lock();
        state.closed = true;
        let events = state
            .items
            .iter()
            .filter(|item| matches!(item, SinkNotification::Event(_)))
            .count();
        state.items.clear();
        events as u64
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SinkRuntime {
    pub queue_capacity: usize,
    /// How long shutdown lets a worker deliver what is already queued.
    pub drain_deadline: Duration,
}

impl Default for SinkRuntime {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            drain_deadline: DEFAULT_DRAIN_DEADLINE,
        }
    }
}

/// Attach a sink to the bus and register its status. The subscription starts
/// before this function returns, so no later event is missed. Cancelling
/// `shutdown` stops intake, delivers the queue until the drain deadline and
/// counts whatever remains as dropped. Wait on `tracker` to observe the end.
pub fn spawn_sink(
    bus: &EventBus,
    sink: Arc<dyn EventSink>,
    status: Arc<SinkStatus>,
    wants: impl Fn(&Event) -> bool + Send + 'static,
    runtime: SinkRuntime,
    shutdown: &CancellationToken,
    tracker: &TaskTracker,
) {
    bus.sink_statuses().register(Arc::clone(&status));
    let queue = Arc::new(Queue {
        state: Mutex::default(),
        notify: Notify::new(),
        capacity: runtime.queue_capacity.max(2),
    });
    let subscription = bus.subscribe();
    tracker.spawn(guarded(
        "forwarder",
        Arc::clone(&status),
        Arc::clone(&queue),
        forward(
            bus.clone(),
            subscription,
            Arc::clone(&queue),
            Arc::clone(&status),
            wants,
            shutdown.clone(),
        ),
    ));
    tracker.spawn(guarded(
        "worker",
        Arc::clone(&status),
        Arc::clone(&queue),
        work(
            queue,
            sink,
            status,
            shutdown.clone(),
            runtime.drain_deadline,
        ),
    ));
}

async fn guarded(
    role: &str,
    status: Arc<SinkStatus>,
    queue: Arc<Queue>,
    task: impl std::future::Future<Output = ()>,
) {
    if AssertUnwindSafe(task).catch_unwind().await.is_err() {
        status.record_stopped(&format!("the sink {role} panicked and stopped"));
        status.record_dropped(queue.discard());
    }
}

async fn forward(
    bus: EventBus,
    mut subscription: crate::EventSubscription,
    queue: Arc<Queue>,
    status: Arc<SinkStatus>,
    wants: impl Fn(&Event) -> bool,
    shutdown: CancellationToken,
) {
    loop {
        let received = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            received = subscription.recv() => received,
        };
        if !intake(received, &bus, &mut subscription, &queue, &status, &wants) {
            break;
        }
    }
    while let Some(received) = subscription.recv().now_or_never() {
        if !intake(received, &bus, &mut subscription, &queue, &status, &wants) {
            break;
        }
    }
    queue.close();
}

/// Returns false when the bus is closed.
fn intake(
    received: std::result::Result<EventEnvelope, ReceiveError>,
    bus: &EventBus,
    subscription: &mut crate::EventSubscription,
    queue: &Queue,
    status: &SinkStatus,
    wants: &impl Fn(&Event) -> bool,
) -> bool {
    match received {
        Ok(envelope) => {
            if wants(&envelope.event) {
                queue.push_event(envelope, status);
            }
            true
        }
        Err(ReceiveError::ResyncRequired(reason)) => {
            status.record_resync();
            queue.push_gap(reason);
            *subscription = bus.subscribe();
            true
        }
        Err(ReceiveError::Closed) => false,
    }
}

async fn work(
    queue: Arc<Queue>,
    sink: Arc<dyn EventSink>,
    status: Arc<SinkStatus>,
    shutdown: CancellationToken,
    drain: Duration,
) {
    let mut deadline: Option<Instant> = None;
    loop {
        let next = if deadline.is_some() {
            queue.pop().await
        } else {
            tokio::select! {
                next = queue.pop() => next,
                () = shutdown.cancelled() => {
                    deadline = Some(Instant::now() + drain);
                    continue;
                }
            }
        };
        let Some(item) = next else { return };
        let mut delivery = std::pin::pin!(deliver(sink.as_ref(), &status, &item));
        let finished = loop {
            match deadline {
                Some(deadline) => {
                    break tokio::time::timeout_at(deadline, &mut delivery)
                        .await
                        .is_ok()
                }
                None => {
                    tokio::select! {
                        () = &mut delivery => break true,
                        () = shutdown.cancelled() => deadline = Some(Instant::now() + drain),
                    }
                }
            }
        };
        if !finished || deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let in_flight = u64::from(!finished && matches!(item, SinkNotification::Event(_)));
            status.record_dropped(in_flight + queue.discard());
            return;
        }
    }
}

async fn deliver(sink: &dyn EventSink, status: &SinkStatus, item: &SinkNotification) {
    match AssertUnwindSafe(sink.handle(item)).catch_unwind().await {
        Ok(Ok(())) => {
            if matches!(item, SinkNotification::Event(_)) {
                status.record_delivered();
            }
        }
        Ok(Err(error)) => status.record_failure(&error.to_string()),
        Err(_) => status.record_failure("the sink panicked while handling a notification"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::sync::Semaphore;

    fn event(n: u64) -> Event {
        Event::SettingsChanged {
            settings: json!({"n": n}),
        }
    }

    #[derive(Default)]
    struct Recorder {
        ids: Mutex<Vec<u64>>,
        gaps: Mutex<Vec<ResyncReason>>,
        gate: Option<Arc<Semaphore>>,
        panic_on: Option<u64>,
    }

    #[async_trait]
    impl EventSink for Recorder {
        async fn handle(&self, notification: &SinkNotification) -> Result<()> {
            if let Some(gate) = &self.gate {
                gate.acquire().await.unwrap().forget();
            }
            match notification {
                SinkNotification::Event(envelope) => {
                    assert_ne!(Some(envelope.id), self.panic_on, "injected panic");
                    self.ids.lock().unwrap().push(envelope.id);
                }
                SinkNotification::ResyncRequired(reason) => {
                    self.gaps.lock().unwrap().push(reason.clone());
                }
            }
            Ok(())
        }
    }

    struct Attached {
        bus: EventBus,
        status: Arc<SinkStatus>,
        shutdown: CancellationToken,
        tracker: TaskTracker,
    }

    fn attach(
        bus: EventBus,
        sink: Arc<dyn EventSink>,
        runtime: SinkRuntime,
        wants: impl Fn(&Event) -> bool + Send + 'static,
    ) -> Attached {
        let status = SinkStatus::new("test", "recorder");
        let shutdown = CancellationToken::new();
        let tracker = TaskTracker::new();
        spawn_sink(
            &bus,
            sink,
            Arc::clone(&status),
            wants,
            runtime,
            &shutdown,
            &tracker,
        );
        Attached {
            bus,
            status,
            shutdown,
            tracker,
        }
    }

    async fn settle(attached: &Attached) {
        attached.shutdown.cancel();
        attached.tracker.close();
        tokio::time::timeout(Duration::from_secs(5), attached.tracker.wait())
            .await
            .expect("sink tasks end");
    }

    #[test]
    fn hmac_matches_rfc_4231_vectors() {
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_sha256_hex(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn error_text_is_bounded_and_loses_the_secret() {
        let text = format!("failed with hunter2\n{}", "x".repeat(1000));
        let clean = redact_and_bound(&text, Some("hunter2"));
        assert!(!clean.contains("hunter2") && clean.contains("<redacted>"));
        assert!(clean.chars().count() <= MAX_ERROR_CHARS + 1 && !clean.contains('\n'));
    }

    #[tokio::test]
    async fn events_are_delivered_in_order_and_filtered() {
        let recorder = Arc::new(Recorder::default());
        let attached = attach(
            EventBus::default(),
            recorder.clone(),
            SinkRuntime::default(),
            |event| event.kind() == "settings_changed",
        );
        for n in 0..10 {
            attached.bus.publish(event(n));
            attached.bus.publish(Event::BackendHealthChanged {
                backend: "b".into(),
                health: None,
                error: None,
            });
        }
        settle(&attached).await;
        assert_eq!(
            *recorder.ids.lock().unwrap(),
            (1..=20).step_by(2).collect::<Vec<_>>()
        );
        let snapshot = attached.status.snapshot();
        assert_eq!((snapshot.delivered, snapshot.state), (10, SinkState::Ok));
    }

    #[tokio::test]
    async fn a_stalled_sink_never_blocks_publishing_and_drops_are_counted() {
        let gate = Arc::new(Semaphore::new(0));
        let recorder = Arc::new(Recorder {
            gate: Some(Arc::clone(&gate)),
            ..Recorder::default()
        });
        let attached = attach(
            EventBus::default(),
            recorder.clone(),
            SinkRuntime {
                queue_capacity: 8,
                drain_deadline: Duration::from_millis(100),
            },
            |_| true,
        );
        let mut slowest = Duration::ZERO;
        for n in 0..3000 {
            let started = std::time::Instant::now();
            attached.bus.publish(event(n));
            slowest = slowest.max(started.elapsed());
            tokio::task::yield_now().await;
        }
        assert!(
            slowest < Duration::from_millis(50),
            "publish took {slowest:?}"
        );
        settle(&attached).await;
        let snapshot = attached.status.snapshot();
        assert!(snapshot.dropped > 2900, "{snapshot:?}");
        assert_eq!(snapshot.delivered, 0);
        assert_eq!(snapshot.state, SinkState::Degraded);
    }

    #[tokio::test]
    async fn a_dropped_event_is_followed_by_an_explicit_gap_marker() {
        let gate = Arc::new(Semaphore::new(0));
        let recorder = Arc::new(Recorder {
            gate: Some(Arc::clone(&gate)),
            ..Recorder::default()
        });
        let attached = attach(
            EventBus::default(),
            recorder.clone(),
            SinkRuntime {
                queue_capacity: 4,
                drain_deadline: Duration::from_secs(2),
            },
            |_| true,
        );
        for n in 0..10 {
            attached.bus.publish(event(n));
            tokio::task::yield_now().await;
        }
        gate.add_permits(100);
        tokio::time::sleep(Duration::from_millis(50)).await;
        attached.bus.publish(event(99));
        settle(&attached).await;
        let ids = recorder.ids.lock().unwrap().clone();
        assert_eq!(ids.last(), Some(&11));
        assert!(ids.len() < 11);
        let gaps = recorder.gaps.lock().unwrap().clone();
        let skipped: u64 = gaps
            .iter()
            .map(|gap| match gap {
                ResyncReason::Lagged { skipped } => *skipped,
                other => panic!("{other:?}"),
            })
            .sum();
        assert_eq!(skipped + ids.len() as u64, 11, "{ids:?} {gaps:?}");
        assert_eq!(attached.status.snapshot().dropped, skipped);
    }

    #[tokio::test]
    async fn a_bus_lag_resubscribes_instead_of_ending_the_sink() {
        let recorder = Arc::new(Recorder::default());
        let attached = attach(
            EventBus::new(8),
            recorder.clone(),
            SinkRuntime::default(),
            |_| true,
        );
        for n in 0..200 {
            attached.bus.publish(event(n));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        attached.bus.publish(event(1000));
        settle(&attached).await;
        let snapshot = attached.status.snapshot();
        assert!(snapshot.resyncs >= 1 && snapshot.last_resync_at.is_some());
        let ids = recorder.ids.lock().unwrap().clone();
        assert_eq!(ids.last(), Some(&201), "delivery continued after the lag");
        let gaps = recorder.gaps.lock().unwrap().len();
        assert!(gaps >= 1);
        assert_eq!(snapshot.state, SinkState::Degraded);
    }

    #[tokio::test]
    async fn a_panicking_sink_keeps_forwarding_and_reports_failures() {
        let recorder = Arc::new(Recorder {
            panic_on: Some(2),
            ..Recorder::default()
        });
        let attached = attach(
            EventBus::default(),
            recorder.clone(),
            SinkRuntime::default(),
            |_| true,
        );
        for n in 0..4 {
            attached.bus.publish(event(n));
        }
        settle(&attached).await;
        assert_eq!(*recorder.ids.lock().unwrap(), vec![1, 3, 4]);
        let snapshot = attached.status.snapshot();
        assert_eq!((snapshot.failed, snapshot.delivered), (1, 3));
        assert!(snapshot.last_error.unwrap().contains("panicked"));
    }

    #[tokio::test]
    async fn consecutive_failures_make_a_sink_failing_until_a_success() {
        let status = SinkStatus::new("s", "k");
        assert_eq!(status.snapshot().state, SinkState::Ok);
        status.record_failure("boom");
        assert_eq!(status.snapshot().state, SinkState::Degraded);
        for _ in 0..FAILING_THRESHOLD {
            status.record_failure("boom");
        }
        assert_eq!(status.snapshot().state, SinkState::Failing);
        status.record_delivered();
        let snapshot = status.snapshot();
        assert_eq!(snapshot.state, SinkState::Degraded);
        assert!(snapshot.last_success_at.is_some());
    }

    #[tokio::test]
    async fn shutdown_drains_the_queue_then_counts_the_rest_as_dropped() {
        let recorder = Arc::new(Recorder::default());
        let attached = attach(
            EventBus::default(),
            recorder.clone(),
            SinkRuntime::default(),
            |_| true,
        );
        for n in 0..50 {
            attached.bus.publish(event(n));
        }
        settle(&attached).await;
        assert_eq!(recorder.ids.lock().unwrap().len(), 50);
        assert_eq!(attached.status.snapshot().dropped, 0);

        let gate = Arc::new(Semaphore::new(0));
        let stalled = Arc::new(Recorder {
            gate: Some(gate),
            ..Recorder::default()
        });
        let attached = attach(
            EventBus::default(),
            stalled,
            SinkRuntime {
                queue_capacity: 64,
                drain_deadline: Duration::from_millis(100),
            },
            |_| true,
        );
        for n in 0..10 {
            attached.bus.publish(event(n));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let started = std::time::Instant::now();
        settle(&attached).await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(attached.status.snapshot().dropped, 10);
    }

    #[tokio::test]
    async fn the_jsonl_sink_writes_exact_lines_a_gap_marker_and_repairs_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"torn\":").unwrap();
        let sink = Arc::new(JsonlSink::new(path.clone()).unwrap());
        let attached = attach(EventBus::new(8), sink, SinkRuntime::default(), |_| true);
        for n in 0..200 {
            attached.bus.publish(event(n));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let last = attached.bus.publish(event(1000));
        settle(&attached).await;
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "{\"torn\":");
        let parsed: Vec<Value> = lines[1..]
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let marker = parsed
            .iter()
            .find(|line| line["type"] == "sink_gap")
            .expect("gap marker");
        assert_eq!(marker["reason"], "lagged");
        assert!(marker["skipped"].as_u64().unwrap() > 0);
        assert!(marker["at"].is_string());
        let final_line = parsed.last().unwrap();
        assert_eq!(
            *final_line,
            serde_json::to_value(&last).unwrap(),
            "the exact envelope is the last line"
        );
        assert_eq!(final_line["type"], "settings_changed");
        assert_eq!(final_line["id"], 201);
    }

    #[tokio::test]
    async fn the_jsonl_sink_bounds_line_size_and_reports_the_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.jsonl");
        let sink = JsonlSink::new(path.clone()).unwrap();
        let bus = EventBus::default();
        let envelope = bus.publish(Event::SettingsChanged {
            settings: json!({"blob": "x".repeat(MAX_LINE_BYTES)}),
        });
        let error = sink
            .handle(&SinkNotification::Event(Box::new(envelope)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("line limit"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() < 1024);
        assert_eq!(
            serde_json::from_str::<Value>(text.trim()).unwrap()["type"],
            "sink_oversize"
        );
    }

    #[test]
    fn jsonl_sink_rejects_a_directory_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(JsonlSink::new(dir.path().to_path_buf()).is_err());
    }
}
