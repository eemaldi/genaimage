use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::post,
    Router,
};
use genaimage_core::{
    config::SinkSecret,
    sinks::{hmac_sha256_hex, spawn_sink, SinkRuntime, SinkState, SinkStatus},
    Event, EventBus, EventEnvelope, EventSink, ResyncReason, SinkNotification,
};
use genaimage_webhook::{WebhookOptions, WebhookSink};
use serde_json::{json, Value};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const SECRET: &str = "TOPSECRET-d1ce";

type Recorded = Vec<(HeaderMap, Vec<u8>)>;

#[derive(Clone, Default)]
struct Receiver {
    script: Arc<Mutex<VecDeque<u16>>>,
    requests: Arc<Mutex<Recorded>>,
    delay: Arc<Mutex<Duration>>,
    elsewhere: Arc<Mutex<usize>>,
}

async fn hook(State(receiver): State<Receiver>, headers: HeaderMap, body: Bytes) -> Response {
    receiver
        .requests
        .lock()
        .unwrap()
        .push((headers, body.to_vec()));
    let delay = *receiver.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    let code = receiver.script.lock().unwrap().pop_front().unwrap_or(200);
    StatusCode::from_u16(code).unwrap().into_response()
}

async fn redirect() -> Redirect {
    Redirect::temporary("/elsewhere")
}

async fn elsewhere(State(receiver): State<Receiver>) -> StatusCode {
    *receiver.elsewhere.lock().unwrap() += 1;
    StatusCode::OK
}

async fn serve(receiver: &Receiver) -> String {
    let app = Router::new()
        .route("/hook", post(hook))
        .route("/redirect", post(redirect))
        .route("/elsewhere", post(elsewhere))
        .with_state(receiver.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

struct Fixture {
    receiver: Receiver,
    base: String,
    dir: tempfile::TempDir,
    status: Arc<SinkStatus>,
}

impl Fixture {
    async fn new() -> Self {
        let receiver = Receiver::default();
        Self {
            base: serve(&receiver).await,
            receiver,
            dir: tempfile::tempdir().unwrap(),
            status: SinkStatus::new("hook", "webhook"),
        }
    }

    fn sink(&self, path: &str, tune: impl FnOnce(&mut WebhookOptions)) -> WebhookSink {
        let mut options = WebhookOptions {
            name: "hook".into(),
            url: format!("{}{path}", self.base),
            allowed_hosts: vec![],
            secret: Some(SinkSecret::new(SECRET.into())),
            timeout: Duration::from_secs(2),
            max_attempts: 5,
            dead_letter_path: self.dir.path().join("hook.dead-letter.jsonl"),
            backoff_base: Duration::from_millis(5),
        };
        tune(&mut options);
        WebhookSink::new(options, Arc::clone(&self.status)).unwrap()
    }

    fn dead_letters(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("hook.dead-letter.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn requests(&self) -> Recorded {
        self.receiver.requests.lock().unwrap().clone()
    }
}

fn envelope() -> EventEnvelope {
    EventBus::default().publish(Event::SettingsChanged {
        settings: json!({"seed": 18446744073709551615_u64, "cfg": 2.0}),
    })
}

fn notification(envelope: &EventEnvelope) -> SinkNotification {
    SinkNotification::Event(Box::new(envelope.clone()))
}

#[tokio::test]
async fn delivers_the_envelope_with_idempotency_and_a_verifiable_signature() {
    let fixture = Fixture::new().await;
    let envelope = envelope();
    fixture
        .sink("/hook", |_| {})
        .handle(&notification(&envelope))
        .await
        .unwrap();
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let (headers, body) = &requests[0];
    let text = |name: &str| headers.get(name).unwrap().to_str().unwrap().to_owned();
    assert_eq!(text("content-type"), "application/json");
    assert_eq!(
        text("idempotency-key"),
        format!("{}:{}", envelope.bus_id, envelope.id)
    );
    assert_eq!(text("x-genaimage-event"), "settings_changed");
    assert_eq!(text("x-genaimage-attempt"), "1");
    assert_eq!(*body, serde_json::to_vec(&envelope).unwrap());
    assert!(String::from_utf8_lossy(body).contains("18446744073709551615"));
    let mut signed = format!("{}.", text("x-genaimage-timestamp")).into_bytes();
    signed.extend_from_slice(body);
    assert_eq!(
        text("x-genaimage-signature"),
        format!("sha256={}", hmac_sha256_hex(SECRET.as_bytes(), &signed))
    );
    assert!(fixture.dead_letters().is_empty());
}

#[tokio::test]
async fn no_secret_means_no_signature_headers() {
    let fixture = Fixture::new().await;
    fixture
        .sink("/hook", |options| options.secret = None)
        .handle(&notification(&envelope()))
        .await
        .unwrap();
    let headers = &fixture.requests()[0].0;
    assert!(headers.get("x-genaimage-signature").is_none());
    assert!(headers.get(header::AUTHORIZATION).is_none());
}

#[tokio::test]
async fn server_errors_and_rate_limits_are_retried_with_the_same_event_id() {
    let fixture = Fixture::new().await;
    fixture
        .receiver
        .script
        .lock()
        .unwrap()
        .extend([503, 429, 408]);
    let envelope = envelope();
    fixture
        .sink("/hook", |_| {})
        .handle(&notification(&envelope))
        .await
        .unwrap();
    let requests = fixture.requests();
    assert_eq!(requests.len(), 4);
    let keys: Vec<_> = requests
        .iter()
        .map(|(headers, _)| headers["idempotency-key"].to_str().unwrap().to_owned())
        .collect();
    assert!(keys.iter().all(|key| *key == keys[0]));
    let attempts: Vec<_> = requests
        .iter()
        .map(|(headers, _)| headers["x-genaimage-attempt"].to_str().unwrap().to_owned())
        .collect();
    assert_eq!(attempts, ["1", "2", "3", "4"]);
    assert_eq!(fixture.status.snapshot().retries, 3);
    assert!(fixture.dead_letters().is_empty());
}

#[tokio::test]
async fn a_client_error_is_permanent_and_dead_lettered_without_retry() {
    let fixture = Fixture::new().await;
    fixture.receiver.script.lock().unwrap().push_back(400);
    let envelope = envelope();
    let error = fixture
        .sink("/hook", |_| {})
        .handle(&notification(&envelope))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("400"), "{error}");
    assert_eq!(fixture.requests().len(), 1);
    let letters = fixture.dead_letters();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0]["permanent"], true);
    assert_eq!(letters[0]["attempts"], 1);
    assert_eq!(letters[0]["sink"], "hook");
    assert!(letters[0]["last_error"].as_str().unwrap().contains("400"));
    assert_eq!(
        letters[0]["event"],
        serde_json::to_value(&envelope).unwrap()
    );
    assert_eq!(fixture.status.snapshot().dead_lettered, 1);
}

#[tokio::test]
async fn retries_are_bounded_and_exhaustion_is_dead_lettered() {
    let fixture = Fixture::new().await;
    fixture.receiver.script.lock().unwrap().extend([500; 10]);
    let error = fixture
        .sink("/hook", |options| options.max_attempts = 3)
        .handle(&notification(&envelope()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("3 attempt"), "{error}");
    assert_eq!(fixture.requests().len(), 3);
    let letters = fixture.dead_letters();
    assert_eq!(
        (
            letters[0]["permanent"].clone(),
            letters[0]["attempts"].clone()
        ),
        (json!(false), json!(3))
    );
}

#[tokio::test]
async fn a_slow_receiver_times_out_and_is_retried_then_dead_lettered() {
    let fixture = Fixture::new().await;
    *fixture.receiver.delay.lock().unwrap() = Duration::from_millis(600);
    let started = std::time::Instant::now();
    fixture
        .sink("/hook", |options| {
            options.timeout = Duration::from_millis(100);
            options.max_attempts = 2;
        })
        .handle(&notification(&envelope()))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fixture.requests().len(), 2);
    assert_eq!(fixture.dead_letters()[0]["attempts"], 2);
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let fixture = Fixture::new().await;
    let error = fixture
        .sink("/redirect", |_| {})
        .handle(&notification(&envelope()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("redirect"), "{error}");
    assert_eq!(*fixture.receiver.elsewhere.lock().unwrap(), 0);
    assert_eq!(fixture.dead_letters()[0]["permanent"], true);
}

#[tokio::test]
async fn a_closed_port_is_a_retryable_connection_error() {
    let fixture = Fixture::new().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let sink = fixture.sink("/hook", |options| {
        options.url = format!("http://127.0.0.1:{port}/hook");
        options.max_attempts = 2;
    });
    sink.handle(&notification(&envelope())).await.unwrap_err();
    assert_eq!(fixture.status.snapshot().retries, 1);
    assert_eq!(fixture.dead_letters()[0]["permanent"], false);
}

#[tokio::test]
async fn public_endpoints_are_refused() {
    let fixture = Fixture::new().await;
    for url in [
        "https://example.com/hook",
        "http://93.184.216.34/hook",
        "http://hook.internal/x",
    ] {
        let options = WebhookOptions {
            name: "hook".into(),
            url: url.into(),
            allowed_hosts: vec![],
            secret: None,
            timeout: Duration::from_secs(1),
            max_attempts: 1,
            dead_letter_path: fixture.dir.path().join("d.jsonl"),
            backoff_base: Duration::from_millis(1),
        };
        assert!(
            WebhookSink::new(options, Arc::clone(&fixture.status)).is_err(),
            "{url}"
        );
    }
}

#[tokio::test]
async fn the_secret_never_appears_in_debug_errors_status_or_dead_letters() {
    let fixture = Fixture::new().await;
    fixture.receiver.script.lock().unwrap().extend([500, 400]);
    let sink = fixture.sink("/hook", |options| options.max_attempts = 3);
    let error = sink
        .handle(&notification(&envelope()))
        .await
        .unwrap_err()
        .to_string();
    let dead_letters =
        std::fs::read_to_string(fixture.dir.path().join("hook.dead-letter.jsonl")).unwrap();
    let status = serde_json::to_string(&fixture.status.snapshot()).unwrap();
    let all = format!(
        "{sink:?} {error} {dead_letters} {status} {:?}",
        SinkSecret::new(SECRET.into())
    );
    assert!(!all.contains(SECRET), "{all}");
    assert!(all.contains("<redacted>"));
}

#[tokio::test]
async fn gap_notifications_are_not_posted() {
    let fixture = Fixture::new().await;
    fixture
        .sink("/hook", |_| {})
        .handle(&SinkNotification::ResyncRequired(ResyncReason::Lagged {
            skipped: 3,
        }))
        .await
        .unwrap();
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn the_runtime_delivers_bus_events_and_reports_status() {
    let fixture = Fixture::new().await;
    fixture.receiver.script.lock().unwrap().push_back(503);
    let bus = EventBus::default();
    let shutdown = CancellationToken::new();
    let tracker = TaskTracker::new();
    spawn_sink(
        &bus,
        Arc::new(fixture.sink("/hook", |_| {})),
        Arc::clone(&fixture.status),
        |event| event.kind() == "settings_changed",
        SinkRuntime::default(),
        &shutdown,
        &tracker,
    );
    let ids: Vec<u64> = (0..5)
        .map(|n| {
            bus.publish(Event::SettingsChanged {
                settings: json!({"n": n}),
            })
            .id
        })
        .collect();
    shutdown.cancel();
    tracker.close();
    tokio::time::timeout(Duration::from_secs(5), tracker.wait())
        .await
        .unwrap();
    let delivered: Vec<String> = fixture
        .requests()
        .iter()
        .map(|(headers, _)| {
            headers["idempotency-key"]
                .to_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    let expected: Vec<String> = [1, 1]
        .iter()
        .chain(&[2, 3, 4, 5])
        .map(|n| ids[n - 1].to_string())
        .collect();
    assert_eq!(
        delivered, expected,
        "order is preserved; the first event is retried once"
    );
    let snapshot = bus.sink_statuses().snapshots();
    assert_eq!(snapshot[0].delivered, 5);
    assert_eq!((snapshot[0].retries, snapshot[0].state), (1, SinkState::Ok));
}
