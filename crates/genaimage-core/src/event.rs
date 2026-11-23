//! In-process events, bounded replay and non-blocking live delivery.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::{
    image_backend::{BackendEvent, BackendHealth},
    persistence::{OutputRecord, RunRecord},
};

/// The event payload. The `type` discriminator and field names form the JSON contract.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunQueued {
        run: RunRecord,
    },
    RunStarted {
        run: RunRecord,
    },
    RunProgress {
        run: RunRecord,
        backend_event: Option<BackendEvent>,
    },
    RunOutput {
        run_id: String,
        output: OutputRecord,
    },
    RunCompleted {
        run: RunRecord,
    },
    RunFailed {
        run: RunRecord,
    },
    RunCancelled {
        run: RunRecord,
    },
    RunUnknownSubmission {
        run: RunRecord,
    },
    BackendHealthChanged {
        backend: String,
        health: Option<BackendHealth>,
        error: Option<String>,
    },
    SettingsChanged {
        settings: Value,
    },
}

/// IDs start at one and are unique and increasing for the lifetime of a bus.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventEnvelope {
    pub bus_id: Uuid,
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
}

/// A replay position bound to the bus lifetime that produced it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventCursor {
    pub bus_id: Uuid,
    pub id: u64,
}

impl EventEnvelope {
    pub fn cursor(&self) -> EventCursor {
        EventCursor {
            bus_id: self.bus_id,
            id: self.id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    CursorOutOfRange {
        requested: u64,
        oldest: Option<u64>,
        latest: u64,
    },
    Lagged {
        skipped: u64,
    },
    /// The cursor belongs to a different bus lifetime.
    BusChanged {
        requested: Uuid,
        current: Uuid,
    },
    /// A state change may have committed without a corresponding event.
    Invalidated {
        id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    ResyncRequired(ResyncReason),
    Closed,
}

struct State {
    latest: u64,
    invalidated_at: u64,
    history: VecDeque<EventEnvelope>,
    progress_subscribers: Vec<(String, mpsc::UnboundedSender<BusMessage>)>,
}

#[derive(Clone)]
enum BusMessage {
    Event(Arc<EventEnvelope>),
    Invalidated(u64),
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) enum RunLockKind {
    Reconcile,
    Cancel,
    SubmitKey,
}

type RunLockKey = (PathBuf, String, RunLockKind);

struct Inner {
    id: Uuid,
    run_locks: Mutex<HashMap<RunLockKey, Weak<AsyncMutex<()>>>>,
    capacity: usize,
    state: Mutex<State>,
    sender: broadcast::Sender<BusMessage>,
}

/// Publishing never waits for a subscriber or an event sink.
#[derive(Clone)]
pub struct EventBus(Arc<Inner>);

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl EventBus {
    /// The replay window holds `capacity` events; Tokio may round up the live capacity.
    /// Panics if the capacity is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "event bus capacity must be positive");
        let (sender, _) = broadcast::channel(capacity);
        Self(Arc::new(Inner {
            id: Uuid::new_v4(),
            run_locks: Mutex::new(HashMap::new()),
            capacity,
            state: Mutex::new(State {
                latest: 0,
                invalidated_at: 0,
                history: VecDeque::with_capacity(capacity),
                progress_subscribers: Vec::new(),
            }),
            sender,
        }))
    }

    pub(crate) fn run_lock(
        &self,
        database: &Path,
        run_id: &str,
        kind: RunLockKind,
    ) -> Arc<AsyncMutex<()>> {
        let key = (database.to_path_buf(), run_id.to_owned(), kind);
        let mut locks = self.0.run_locks.lock().expect("run lock map poisoned");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    pub fn publish(&self, event: Event) -> EventEnvelope {
        let mut state = self.0.state.lock().expect("event bus mutex poisoned");
        state.latest = state.latest.checked_add(1).expect("event bus ID exhausted");
        let envelope = EventEnvelope {
            bus_id: self.0.id,
            id: state.latest,
            timestamp: Utc::now(),
            event,
        };
        if state.history.len() == self.0.capacity {
            state.history.pop_front();
        }
        state.history.push_back(envelope.clone());
        let message = BusMessage::Event(Arc::new(envelope.clone()));
        let _ = self.0.sender.send(message.clone());
        let progress_run = match &envelope.event {
            Event::RunProgress { run, .. } => Some(run.id.as_str()),
            _ => None,
        };
        state.progress_subscribers.retain(|(run_id, sender)| {
            !sender.is_closed()
                && (progress_run != Some(run_id.as_str()) || sender.send(message.clone()).is_ok())
        });
        envelope
    }

    /// Mark the current stream unreliable and force existing consumers to resynchronise.
    /// A reserved ID prevents replay across the invalidation boundary.
    pub fn invalidate(&self) {
        let mut state = self.0.state.lock().expect("event bus mutex poisoned");
        state.latest = state.latest.checked_add(1).expect("event bus ID exhausted");
        state.invalidated_at = state.latest;
        state.history.clear();
        let message = BusMessage::Invalidated(state.latest);
        let _ = self.0.sender.send(message.clone());
        state
            .progress_subscribers
            .retain(|(_, sender)| sender.send(message.clone()).is_ok());
    }

    /// Return the current cursor, including this bus lifetime's identifier.
    pub fn cursor(&self) -> EventCursor {
        let state = self.0.state.lock().expect("event bus mutex poisoned");
        EventCursor {
            bus_id: self.0.id,
            id: state.latest,
        }
    }

    /// Replay from a lifetime-bound cursor. Cursors from another bus require resync.
    pub fn subscribe_from_cursor(
        &self,
        cursor: EventCursor,
    ) -> std::result::Result<EventSubscription, ResyncReason> {
        if cursor.bus_id != self.0.id {
            return Err(ResyncReason::BusChanged {
                requested: cursor.bus_id,
                current: self.0.id,
            });
        }
        self.subscribe_since(cursor.id)
    }

    /// Subscribe to future events only.
    pub fn subscribe(&self) -> EventSubscription {
        let _state = self.0.state.lock().expect("event bus mutex poisoned");
        EventSubscription {
            replay: VecDeque::new(),
            receiver: self.0.sender.subscribe(),
            terminal: false,
        }
    }

    /// Subscribe to every future progress update for one run without dropping any.
    /// This deliberately uses an unbounded queue to preserve the CLI's existing
    /// progress output. It does not replay events or deliver other event kinds.
    /// An invalidation is reported but does not close this progress-only stream.
    pub fn subscribe_run_progress(&self, run_id: &str) -> ProgressSubscription {
        let mut state = self.0.state.lock().expect("event bus mutex poisoned");
        let (sender, receiver) = mpsc::unbounded_channel();
        state
            .progress_subscribers
            .retain(|(_, sender)| !sender.is_closed());
        state.progress_subscribers.push((run_id.to_owned(), sender));
        ProgressSubscription { receiver }
    }

    /// Replay events strictly after `id`, then receive live events without a gap.
    /// A cursor before the retained window, across an invalidation, or ahead
    /// of the bus requires resync.
    /// Cursors are valid only for the lifetime of the originating bus.
    pub fn subscribe_since(&self, id: u64) -> std::result::Result<EventSubscription, ResyncReason> {
        let state = self.0.state.lock().expect("event bus mutex poisoned");
        let oldest = state.history.front().map(|event| event.id);
        if id < state.invalidated_at {
            return Err(ResyncReason::Invalidated {
                id: state.invalidated_at,
            });
        }
        if id > state.latest || oldest.is_some_and(|first| id < first - 1) {
            return Err(ResyncReason::CursorOutOfRange {
                requested: id,
                oldest,
                latest: state.latest,
            });
        }
        let receiver = self.0.sender.subscribe();
        let replay = state
            .history
            .iter()
            .filter(|event| event.id > id)
            .cloned()
            .collect();
        Ok(EventSubscription {
            replay,
            receiver,
            terminal: false,
        })
    }
}

pub struct ProgressSubscription {
    receiver: mpsc::UnboundedReceiver<BusMessage>,
}

impl ProgressSubscription {
    pub async fn recv(&mut self) -> std::result::Result<EventEnvelope, ReceiveError> {
        match self.receiver.recv().await {
            Some(BusMessage::Event(event)) => Ok((*event).clone()),
            Some(BusMessage::Invalidated(id)) => {
                Err(ReceiveError::ResyncRequired(ResyncReason::Invalidated {
                    id,
                }))
            }
            None => Err(ReceiveError::Closed),
        }
    }
}

pub struct EventSubscription {
    replay: VecDeque<EventEnvelope>,
    receiver: broadcast::Receiver<BusMessage>,
    terminal: bool,
}

impl EventSubscription {
    /// After a resync signal, the subscription terminates: reload state and subscribe again.
    pub async fn recv(&mut self) -> std::result::Result<EventEnvelope, ReceiveError> {
        if self.terminal {
            return Err(ReceiveError::Closed);
        }
        if let Some(event) = self.replay.pop_front() {
            return Ok(event);
        }
        match self.receiver.recv().await {
            Ok(BusMessage::Event(event)) => Ok((*event).clone()),
            Ok(BusMessage::Invalidated(id)) => {
                self.terminal = true;
                Err(ReceiveError::ResyncRequired(ResyncReason::Invalidated {
                    id,
                }))
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                self.terminal = true;
                Err(ReceiveError::ResyncRequired(ResyncReason::Lagged {
                    skipped,
                }))
            }
            Err(broadcast::error::RecvError::Closed) => {
                self.terminal = true;
                Err(ReceiveError::Closed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        async_trait, EventSink, ExtensionRegistry, HarnessError, Result, SinkNotification,
    };
    use serde_json::json;
    use tokio::{
        sync::{mpsc, Semaphore},
        time::{timeout, Duration},
    };

    fn change(n: u64) -> Event {
        Event::SettingsChanged {
            settings: json!({"value": n}),
        }
    }

    #[tokio::test]
    async fn replay_then_live_is_contiguous_and_tagged_json_round_trips() {
        let bus = EventBus::new(4);
        bus.publish(change(1));
        let second = bus.publish(change(2));
        let json = serde_json::to_value(&second).unwrap();
        assert_eq!(json["id"], 2);
        assert_eq!(json["bus_id"], second.bus_id.to_string());
        assert_eq!(json["type"], "settings_changed");
        assert_eq!(json["settings"]["value"], 2);
        assert!(json["timestamp"].is_string());
        let decoded: EventEnvelope = serde_json::from_value(json).unwrap();
        assert!(matches!(decoded.event, Event::SettingsChanged { .. }));
        let mut subscriber = bus.subscribe_since(1).unwrap();
        bus.publish(change(3));
        bus.publish(change(4));
        for expected in 2..=4 {
            assert_eq!(subscriber.recv().await.unwrap().id, expected);
        }
        assert!(timeout(Duration::from_millis(10), subscriber.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn stale_and_future_cursors_require_resync_and_lag_terminates_delivery() {
        let bus = EventBus::new(2);
        let mut subscriber = bus.subscribe();
        for n in 1..=4 {
            bus.publish(change(n));
        }
        assert_eq!(
            bus.subscribe_since(1).err(),
            Some(ResyncReason::CursorOutOfRange {
                requested: 1,
                oldest: Some(3),
                latest: 4
            })
        );
        assert_eq!(
            bus.subscribe_since(5).err(),
            Some(ResyncReason::CursorOutOfRange {
                requested: 5,
                oldest: Some(3),
                latest: 4
            })
        );
        let mut edge = bus.subscribe_since(2).unwrap();
        assert_eq!(edge.recv().await.unwrap().id, 3);
        assert_eq!(edge.recv().await.unwrap().id, 4);
        assert_eq!(
            subscriber.recv().await.err(),
            Some(ReceiveError::ResyncRequired(ResyncReason::Lagged {
                skipped: 2
            }))
        );
        assert_eq!(subscriber.recv().await.err(), Some(ReceiveError::Closed));
    }

    #[tokio::test]
    async fn lifetime_bound_cursors_reject_another_bus_even_when_the_id_fits() {
        let old = EventBus::new(4);
        let cursor = old.publish(change(1)).cursor();
        let new = EventBus::new(4);
        new.publish(change(1));
        assert_eq!(
            new.subscribe_from_cursor(cursor).err(),
            Some(ResyncReason::BusChanged {
                requested: cursor.bus_id,
                current: new.cursor().bus_id
            })
        );
        let cursor = new.cursor();
        let mut subscriber = new.subscribe_from_cursor(cursor).unwrap();
        assert_eq!(new.publish(change(2)).cursor().bus_id, cursor.bus_id);
        assert_eq!(subscriber.recv().await.unwrap().id, 2);
    }

    #[tokio::test]
    async fn invalidation_reaches_live_and_replay_subscribers() {
        let bus = EventBus::new(4);
        bus.publish(change(1));
        let mut replay = bus.subscribe_since(0).unwrap();
        bus.publish(change(2));
        bus.invalidate();
        let next = bus.publish(change(3));
        assert_eq!(next.id, 4);
        assert_eq!(replay.recv().await.unwrap().id, 1);
        assert_eq!(replay.recv().await.unwrap().id, 2);
        assert_eq!(
            replay.recv().await.err(),
            Some(ReceiveError::ResyncRequired(ResyncReason::Invalidated {
                id: 3
            }))
        );
        assert_eq!(replay.recv().await.err(), Some(ReceiveError::Closed));
        assert_eq!(
            bus.subscribe_since(2).err(),
            Some(ResyncReason::Invalidated { id: 3 })
        );
        assert_eq!(bus.subscribe_since(3).unwrap().recv().await.unwrap().id, 4);
        let mut fresh = bus.subscribe();
        assert_eq!(bus.publish(change(4)).id, 5);
        assert_eq!(fresh.recv().await.unwrap().id, 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_publishers_and_atomic_replay_handoff_are_ordered() {
        let bus = EventBus::new(2048);
        let mut publishers = Vec::new();
        for _ in 0..8 {
            let bus = bus.clone();
            publishers.push(tokio::spawn(async move {
                for n in 0..100 {
                    bus.publish(change(n));
                    tokio::task::yield_now().await;
                }
            }));
        }
        let mut subscription = bus.subscribe_since(0).unwrap();
        for publisher in publishers {
            publisher.await.unwrap();
        }
        for expected in 1..=800 {
            assert_eq!(subscription.recv().await.unwrap().id, expected);
        }
    }

    struct SlowSink {
        started: mpsc::UnboundedSender<()>,
        release: Semaphore,
        reports: mpsc::UnboundedSender<SinkNotification>,
    }

    #[async_trait]
    impl EventSink for SlowSink {
        async fn handle(&self, notification: &SinkNotification) -> Result<()> {
            if matches!(notification, SinkNotification::Event(_)) {
                self.started
                    .send(())
                    .map_err(|error| HarnessError::Configuration(error.to_string()))?;
                self.release.acquire().await.unwrap().forget();
            }
            self.reports
                .send(notification.clone())
                .map_err(|error| HarnessError::Configuration(error.to_string()))?;
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_registered_sink_cannot_block_publishers_and_is_told_to_resync() {
        let bus = EventBus::new(2);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (reports_tx, mut reports_rx) = mpsc::unbounded_channel();
        let sink = Arc::new(SlowSink {
            started: started_tx,
            release: Semaphore::new(0),
            reports: reports_tx,
        });
        let mut registry = ExtensionRegistry::default();
        registry.register_event_sink("slow", sink.clone()).unwrap();
        let mut tasks = registry.forward_event_sinks(&bus);
        let mut live = bus.subscribe();
        bus.publish(change(0));
        timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), live.recv())
                .await
                .unwrap()
                .unwrap()
                .id,
            1
        );
        let publisher = {
            let bus = bus.clone();
            tokio::spawn(async move {
                for n in 1..=100 {
                    bus.publish(change(n));
                }
            })
        };
        timeout(Duration::from_secs(1), publisher)
            .await
            .unwrap()
            .unwrap();
        sink.release.add_permits(1);
        assert!(matches!(
            timeout(Duration::from_secs(1), reports_rx.recv())
                .await
                .unwrap(),
            Some(SinkNotification::Event(_))
        ));
        assert!(matches!(
            timeout(Duration::from_secs(1), reports_rx.recv())
                .await
                .unwrap(),
            Some(SinkNotification::ResyncRequired(
                ResyncReason::Lagged { .. }
            ))
        ));
        tasks.remove("slow").unwrap().await.unwrap().unwrap();
    }
}
