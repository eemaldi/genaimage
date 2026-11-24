mod common;

use std::time::Duration;

use axum::{body::Body, http::Request};
use common::ApiHarness;
use futures_util::future::join_all;
use genaimage_core::{persistence::OutputRecord, Event, EventEnvelope, RunRecord, RunStatus};
use genaimage_server::{build_app, AppState};
use http_body_util::BodyExt;
use reqwest::{header, Method, RequestBuilder, Response, StatusCode};
use serde_json::{json, Value};
use tokio::time::timeout;
use tower::ServiceExt;

const EVENTS: &str = "/api/v1/events";
const DEADLINE: Duration = Duration::from_secs(5);

struct SseClient {
    response: Response,
    buffer: Vec<u8>,
}

impl SseClient {
    async fn connect(request: RequestBuilder) -> Self {
        let response = timeout(DEADLINE, request.send()).await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        let mut client = Self {
            response,
            buffer: Vec::new(),
        };
        assert_eq!(client.frame().await.as_deref(), Some(": connected\n\n"));
        client
    }

    async fn frame(&mut self) -> Option<String> {
        timeout(DEADLINE, async {
            loop {
                if let Some(end) = self.buffer.windows(2).position(|bytes| bytes == b"\n\n") {
                    return Some(
                        String::from_utf8(self.buffer.drain(..end + 2).collect()).unwrap(),
                    );
                }
                match self.response.chunk().await.unwrap() {
                    Some(chunk) => self.buffer.extend_from_slice(&chunk),
                    None => {
                        assert!(self.buffer.is_empty(), "incomplete SSE frame");
                        return None;
                    }
                }
            }
        })
        .await
        .expect("SSE frame or EOF deadline")
    }

    async fn assert_event(&mut self, kind: &str, envelope: &EventEnvelope) {
        let frame = self.frame().await.expect("expected core event");
        assert_eq!(field(&frame, "id: "), envelope.id.to_string());
        assert_eq!(field(&frame, "event: "), kind);
        let payload = data(&frame);
        assert_eq!(payload, serde_json::to_value(envelope).unwrap());
        let documented: genaimage_api::EventEnvelope =
            serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(documented).unwrap(), payload);
    }
}

fn field<'a>(frame: &'a str, prefix: &str) -> &'a str {
    frame
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in {frame:?}"))
}

fn data(frame: &str) -> Value {
    serde_json::from_str(field(frame, "data: ")).unwrap()
}

fn assert_resync(frame: &str, reason: &str) -> Value {
    assert_eq!(field(frame, "event: "), "resync");
    assert_eq!(field(frame, "id: "), "");
    let payload = data(frame);
    assert_eq!(payload["reason"], reason);
    let documented: genaimage_api::ResyncEvent = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(serde_json::to_value(documented).unwrap(), payload);
    payload
}

fn publish(api: &ApiHarness, value: u64) -> EventEnvelope {
    api.core.events().publish(Event::SettingsChanged {
        settings: json!({"value": value}),
    })
}

fn run_output(api: &ApiHarness, run_id: &str) -> EventEnvelope {
    api.core.events().publish(Event::RunOutput {
        run_id: run_id.into(),
        output: OutputRecord {
            run_id: run_id.into(),
            node_id: "1".into(),
            kind: "image".into(),
            source_filename: "result.png".into(),
            source_subfolder: String::new(),
            source_type: "output".into(),
            local_path: "result.png".into(),
            sha256: "f".repeat(64),
            byte_size: 10,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        },
    })
}

#[tokio::test]
async fn streams_new_events_without_compression_and_ends_on_shutdown() {
    let api = ApiHarness::start().await;
    publish(&api, 0); // A stream without a cursor does not replay.
    let mut client = SseClient::connect(
        api.request(Method::GET, EVENTS)
            .header(header::ACCEPT_ENCODING, "gzip, br"),
    )
    .await;
    client
        .assert_event("settings_changed", &publish(&api, 1))
        .await;
    let ((), end) = tokio::join!(api.shutdown(), client.frame());
    assert_eq!(end, None);
}

#[tokio::test]
async fn filters_replay_and_live_events_by_run_id() {
    let api = ApiHarness::start().await;
    let cursor = publish(&api, 0);
    run_output(&api, "other");
    let replayed = run_output(&api, "wanted");
    let path = format!(
        "{EVENTS}?run_id=wanted&last_event_id={}&bus_id={}",
        cursor.id, cursor.bus_id
    );
    let mut client = SseClient::connect(api.request(Method::GET, &path)).await;
    publish(&api, 1);
    run_output(&api, "other");
    let now = "2026-01-01T00:00:00Z".parse().unwrap();
    let live = api.core.events().publish(Event::RunQueued {
        run: RunRecord {
            id: "wanted".into(),
            prompt_id: "prompt".into(),
            client_id: "client".into(),
            workflow_id: "workflow".into(),
            workflow_version: "v1".into(),
            workflow_sha256: "f".repeat(64),
            workflow_manifest_sha256: None,
            resolved_workflow_sha256: None,
            rerun_of: None,
            preflight_sha256: None,
            generation_mode: Default::default(),
            status: RunStatus::Queued,
            parameters: json!({}),
            output_dir: "outputs".into(),
            manifest_path: None,
            error: None,
            node_warnings: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        },
    });
    client.assert_event("run_output", &replayed).await;
    client.assert_event("run_queued", &live).await;
}

#[tokio::test]
async fn header_resume_precedes_query_and_hands_over_to_live_delivery() {
    let api = ApiHarness::start().await;
    let cursor = publish(&api, 1);
    let replay = [publish(&api, 2), publish(&api, 3)];
    let mut client = SseClient::connect(
        api.request(Method::GET, &format!("{EVENTS}?last_event_id=0"))
            .header("last-event-id", cursor.id),
    )
    .await;
    let live = publish(&api, 4);
    for event in replay.iter().chain([&live]) {
        client.assert_event("settings_changed", event).await;
    }
}

#[tokio::test]
async fn cursor_from_previous_bus_gets_resync_even_at_current_latest_id() {
    let old_bus = genaimage_core::EventBus::default();
    let old = old_bus.publish(Event::SettingsChanged {
        settings: json!({}),
    });
    let api = ApiHarness::start().await;
    let current = publish(&api, 1);
    assert_eq!(old.id, current.id);
    assert_ne!(old.bus_id, current.bus_id);

    let path = format!("{EVENTS}?last_event_id={}&bus_id={}", old.id, old.bus_id);
    let mut client = SseClient::connect(api.request(Method::GET, &path)).await;
    let payload = assert_resync(&client.frame().await.unwrap(), "bus_changed");
    assert_eq!(payload["requested"], old.bus_id.to_string());
    assert_eq!(payload["current"], current.bus_id.to_string());
    assert_eq!(client.frame().await, None);
}

#[tokio::test]
async fn out_of_range_cursors_get_resync_then_eof() {
    let api = ApiHarness::start().await;
    let first = publish(&api, 0).id;
    for value in 1..=2048 {
        publish(&api, value);
    }
    let latest = api.core.events().cursor().id;
    for requested in [first, latest + 1] {
        let mut client = SseClient::connect(
            api.request(Method::GET, EVENTS)
                .header("last-event-id", requested),
        )
        .await;
        let payload = assert_resync(&client.frame().await.unwrap(), "cursor_out_of_range");
        assert_eq!(payload["requested"], requested);
        assert_eq!(payload["latest"], latest);
        assert!(payload["oldest"].as_u64().unwrap() > first + 1);
        assert_eq!(client.frame().await, None);
    }
}

#[tokio::test]
async fn invalid_cursors_return_problem_details() {
    let api = ApiHarness::start().await;
    for request in [
        api.request(Method::GET, EVENTS)
            .header("last-event-id", "-1"),
        api.request(Method::GET, EVENTS)
            .header("last-event-id", "1")
            .header("last-event-id", "2"),
        api.request(
            Method::GET,
            &format!("{EVENTS}?last_event_id=18446744073709551616"),
        ),
        api.request(Method::GET, &format!("{EVENTS}?run_id=one&run_id=two")),
        api.request(
            Method::GET,
            &format!("{EVENTS}?last_event_id=1&bus_id=invalid"),
        ),
        api.request(
            Method::GET,
            &format!("{EVENTS}?bus_id={}", api.core.events().cursor().bus_id),
        ),
    ] {
        let response = timeout(DEADLINE, request.send()).await.unwrap().unwrap();
        ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    }
}

async fn in_process_body(api: &ApiHarness) -> Body {
    let port = 3000;
    let response = build_app(AppState::new(api.core.clone()), port)
        .oneshot(
            Request::get(EVENTS)
                .header(header::HOST, "127.0.0.1:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body()
}

async fn body_frame(body: &mut Body) -> Option<String> {
    body.frame().await.map(|frame| {
        let bytes = frame.unwrap().into_data().unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    })
}

#[tokio::test]
async fn a_lagging_response_gets_resync_then_eof() {
    let api = ApiHarness::start().await;
    let mut body = in_process_body(&api).await;
    assert_eq!(
        body_frame(&mut body).await.as_deref(),
        Some(": connected\n\n")
    );
    // Leave the body unpolled to deterministically fill the broadcast buffer.
    for value in 0..2048 {
        publish(&api, value);
    }
    let frame = timeout(DEADLINE, body_frame(&mut body))
        .await
        .unwrap()
        .unwrap();
    assert!(assert_resync(&frame, "lagged")["skipped"].as_u64().unwrap() > 0);
    assert_eq!(
        timeout(DEADLINE, body_frame(&mut body)).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn an_idle_stream_sends_periodic_heartbeat_comments() {
    let api = ApiHarness::start().await;
    tokio::time::pause();
    let mut body = in_process_body(&api).await;
    assert_eq!(
        body_frame(&mut body).await.as_deref(),
        Some(": connected\n\n")
    );
    for _ in 0..2 {
        assert!(timeout(Duration::from_secs(14), body_frame(&mut body))
            .await
            .is_err());
        assert_eq!(
            timeout(Duration::from_secs(2), body_frame(&mut body))
                .await
                .unwrap()
                .as_deref(),
            Some(": heartbeat\n\n")
        );
    }
}

#[tokio::test]
async fn ten_idle_http_clients_do_not_block_publishers() {
    let api = ApiHarness::start().await;
    let clients =
        join_all((0..10).map(|_| SseClient::connect(api.request(Method::GET, EVENTS)))).await;
    let bus = api.core.events().clone();
    let start = bus.cursor().id;
    const BURST: u64 = 4096;
    // Never read from these clients while publishing a burst larger than the
    // replay window. Bound completion generously rather than timing each event.
    let publisher = tokio::task::spawn_blocking(move || {
        let settings = json!({"payload": "x".repeat(8192)});
        for _ in 0..BURST {
            bus.publish(Event::SettingsChanged {
                settings: settings.clone(),
            });
        }
        bus.cursor().id
    });
    let result = timeout(DEADLINE, publisher).await;
    drop(clients);
    assert_eq!(
        result
            .expect("publishing blocked on idle SSE clients")
            .unwrap(),
        start + BURST
    );
    api.shutdown().await;
}
