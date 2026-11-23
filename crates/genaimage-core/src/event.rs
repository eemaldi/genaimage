//! In-process events, bounded replay and non-blocking live delivery.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

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
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    ResyncRequired(ResyncReason),
    Closed,
}

struct State {
    latest: u64,
    history: VecDeque<EventEnvelope>,
}

struct Inner {
    capacity: usize,
    state: Mutex<State>,
    sender: broadcast::Sender<EventEnvelope>,
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
            capacity,
            state: Mutex::new(State {
                latest: 0,
                history: VecDeque::with_capacity(capacity),
            }),
            sender,
        }))
    }

    pub fn publish(&self, event: Event) -> EventEnvelope {
        let mut state = self.0.state.lock().expect("event bus mutex poisoned");
        state.latest = state.latest.checked_add(1).expect("event bus ID exhausted");
        let envelope = EventEnvelope {
            id: state.latest,
            timestamp: Utc::now(),
            event,
        };
        if state.history.len() == self.0.capacity {
            state.history.pop_front();
        }
        state.history.push_back(envelope.clone());
        let _ = self.0.sender.send(envelope.clone());
        envelope
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

    /// Replay events strictly after `id`, then receive live events without a gap.
    /// A cursor before the retained window or ahead of the bus requires resync.
    /// Cursors are valid only for the lifetime of the originating bus.
    pub fn subscribe_since(&self, id: u64) -> std::result::Result<EventSubscription, ResyncReason> {
        let state = self.0.state.lock().expect("event bus mutex poisoned");
        let oldest = state.history.front().map(|event| event.id);
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

pub struct EventSubscription {
    replay: VecDeque<EventEnvelope>,
    receiver: broadcast::Receiver<EventEnvelope>,
    terminal: bool,
}

impl EventSubscription {
    /// After lag, the subscription terminates: resynchronise state and subscribe again.
    pub async fn recv(&mut self) -> std::result::Result<EventEnvelope, ReceiveError> {
        if self.terminal {
            return Err(ReceiveError::Closed);
        }
        if let Some(event) = self.replay.pop_front() {
            return Ok(event);
        }
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
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
