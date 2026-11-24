//! Ergonomic scripted fake for OpenAI-compatible LLM adapter tests.

use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::Notify};

/// One streamed response step. JSON steps are encoded as OpenAI SSE `data`
/// frames; `RawSse` permits malformed-protocol tests.
#[derive(Clone, Debug)]
pub enum FakeLlmEvent {
    Json(Value),
    RawSse(String),
    Delay(Duration),
    Hang,
    Done,
}

impl FakeLlmEvent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Json(json!({
            "choices": [{ "delta": { "content": text.into() }, "finish_reason": null }]
        }))
    }

    pub fn tool_calls(calls: impl IntoIterator<Item = FakeToolCallDelta>) -> Self {
        let calls = calls
            .into_iter()
            .map(|call| {
                let mut function = json!({});
                if let Some(name) = call.name {
                    function["name"] = Value::String(name);
                }
                if !call.arguments.is_empty() {
                    function["arguments"] = Value::String(call.arguments);
                }
                let mut value =
                    json!({ "index": call.index, "type": "function", "function": function });
                if let Some(id) = call.id {
                    value["id"] = Value::String(id);
                }
                value
            })
            .collect::<Vec<_>>();
        Self::Json(json!({
            "choices": [{ "delta": { "tool_calls": calls }, "finish_reason": null }]
        }))
    }

    pub fn finish(reason: &str) -> Self {
        Self::Json(json!({ "choices": [{ "delta": {}, "finish_reason": reason }] }))
    }
}

/// A fragment from an OpenAI `tool_calls` stream delta.
#[derive(Clone, Debug)]
pub struct FakeToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

impl FakeToolCallDelta {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            id: None,
            name: None,
            arguments: String::new(),
        }
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn arguments(mut self, arguments: impl Into<String>) -> Self {
        self.arguments = arguments.into();
        self
    }
}

/// A reusable response script. Start with `default()`, replace events with
/// `with_events`, or use `text_chunks`, then optionally configure errors/models.
#[derive(Clone, Debug)]
pub struct FakeLlmScript {
    events: Vec<FakeLlmEvent>,
    models: Value,
    chat_error: Option<(StatusCode, String)>,
    models_error: Option<(StatusCode, String)>,
    chat_response_delay: Duration,
    models_response_delay: Duration,
}

impl Default for FakeLlmScript {
    fn default() -> Self {
        Self::text_chunks(["hello"])
    }
}

impl FakeLlmScript {
    pub fn with_events(mut self, events: impl IntoIterator<Item = FakeLlmEvent>) -> Self {
        self.events = events.into_iter().collect();
        self
    }

    pub fn text_chunks<I, S>(chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut events = chunks
            .into_iter()
            .map(FakeLlmEvent::text)
            .collect::<Vec<_>>();
        events.push(FakeLlmEvent::finish("stop"));
        events.push(FakeLlmEvent::Done);
        Self {
            events,
            models: json!({ "object": "list", "data": [{ "id": "test-model" }] }),
            chat_error: None,
            models_error: None,
            chat_response_delay: Duration::ZERO,
            models_response_delay: Duration::ZERO,
        }
    }

    /// Supplies an exact `/v1/models` JSON response for capability metadata tests.
    pub fn with_models_response(mut self, response: Value) -> Self {
        self.models = response;
        self
    }

    pub fn with_models<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.models = json!({
            "object": "list",
            "data": ids.into_iter().map(|id| json!({ "id": id.into() })).collect::<Vec<_>>()
        });
        self
    }

    /// Delays the model-listing response, exercising probe concurrency and deadlines.
    pub fn with_models_response_delay(mut self, delay: Duration) -> Self {
        self.models_response_delay = delay;
        self
    }

    /// Delays the HTTP response headers, exercising the provider's first-byte timeout.
    pub fn with_response_delay(mut self, delay: Duration) -> Self {
        self.chat_response_delay = delay;
        self
    }

    pub fn with_chat_error(mut self, status: u16, message: impl Into<String>) -> Self {
        self.chat_error = Some((
            StatusCode::from_u16(status).expect("valid fake LLM status"),
            message.into(),
        ));
        self
    }

    pub fn with_models_error(mut self, status: u16, message: impl Into<String>) -> Self {
        self.models_error = Some((
            StatusCode::from_u16(status).expect("valid fake LLM status"),
            message.into(),
        ));
        self
    }
}

#[derive(Clone)]
struct FakeLlmState {
    scripts: Arc<Mutex<Vec<FakeLlmScript>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    chat_requests: Arc<AtomicUsize>,
    models_requests: Arc<AtomicUsize>,
    stream_disconnects: Arc<AtomicUsize>,
    request_arrived: Arc<Notify>,
    stream_dropped: Arc<Notify>,
}

impl FakeLlmState {
    fn from_scripts(scripts: Vec<FakeLlmScript>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(scripts)),
            requests: Arc::new(Mutex::new(Vec::new())),
            chat_requests: Arc::new(AtomicUsize::new(0)),
            models_requests: Arc::new(AtomicUsize::new(0)),
            stream_disconnects: Arc::new(AtomicUsize::new(0)),
            request_arrived: Arc::new(Notify::new()),
            stream_dropped: Arc::new(Notify::new()),
        }
    }
}

async fn fake_chat(State(state): State<FakeLlmState>, Json(body): Json<Value>) -> Response {
    state.requests.lock().expect("request lock").push(body);
    state.chat_requests.fetch_add(1, Ordering::SeqCst);
    state.request_arrived.notify_waiters();
    let script = {
        let mut scripts = state.scripts.lock().expect("script lock");
        if scripts.len() > 1 {
            scripts.remove(0)
        } else {
            scripts.first().expect("fake LLM script").clone()
        }
    };
    if !script.chat_response_delay.is_zero() {
        tokio::time::sleep(script.chat_response_delay).await;
    }
    if let Some((status, message)) = script.chat_error {
        return (status, Json(json!({ "error": { "message": message } }))).into_response();
    }

    let probe = StreamProbe {
        state: state.clone(),
        completed: false,
    };
    let steps = script.events;
    let stream = stream::unfold(
        (steps.into_iter(), probe),
        |(mut steps, probe)| async move {
            loop {
                let Some(step) = steps.next() else {
                    probe.complete();
                    return None;
                };
                match step {
                    FakeLlmEvent::Json(value) => {
                        let bytes = Bytes::from(format!("data: {value}\n\n"));
                        return Some((Ok::<_, Infallible>(bytes), (steps, probe)));
                    }
                    FakeLlmEvent::RawSse(raw) => {
                        return Some((Ok::<_, Infallible>(Bytes::from(raw)), (steps, probe)));
                    }
                    FakeLlmEvent::Delay(delay) => tokio::time::sleep(delay).await,
                    FakeLlmEvent::Hang => std::future::pending::<()>().await,
                    FakeLlmEvent::Done => {
                        let bytes = Bytes::from_static(b"data: [DONE]\n\n");
                        return Some((Ok(bytes), (steps, probe)));
                    }
                }
            }
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .expect("fake LLM stream response")
}

struct StreamProbe {
    state: FakeLlmState,
    completed: bool,
}

impl StreamProbe {
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for StreamProbe {
    fn drop(&mut self) {
        if !self.completed {
            self.state.stream_disconnects.fetch_add(1, Ordering::SeqCst);
            self.state.stream_dropped.notify_waiters();
        }
    }
}

async fn fake_models(State(state): State<FakeLlmState>) -> Response {
    state.models_requests.fetch_add(1, Ordering::SeqCst);
    let script = state
        .scripts
        .lock()
        .expect("script lock")
        .first()
        .expect("fake LLM script")
        .clone();
    if !script.models_response_delay.is_zero() {
        tokio::time::sleep(script.models_response_delay).await;
    }
    if let Some((status, message)) = script.models_error {
        return (status, Json(json!({ "error": { "message": message } }))).into_response();
    }
    Json(script.models).into_response()
}

/// Owns a loopback fake OpenAI-compatible server and aborts it on drop.
pub struct FakeLlmServer {
    address: SocketAddr,
    state: FakeLlmState,
    task: tokio::task::JoinHandle<()>,
}

impl FakeLlmServer {
    pub async fn start(script: FakeLlmScript) -> Self {
        Self::start_sequence([script]).await
    }

    pub async fn start_sequence(scripts: impl IntoIterator<Item = FakeLlmScript>) -> Self {
        let scripts = scripts.into_iter().collect::<Vec<_>>();
        assert!(
            !scripts.is_empty(),
            "at least one fake LLM script is required"
        );
        let state = FakeLlmState::from_scripts(scripts);
        let app = Router::new()
            .route("/v1/chat/completions", post(fake_chat))
            .route("/v1/models", get(fake_models))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake LLM server");
        let address = listener.local_addr().expect("listener address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake LLM server");
        });
        Self {
            address,
            state,
            task,
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// Replaces the script for subsequent requests. The previous requests and
    /// cancellation observations remain available for assertions.
    pub fn set_script(&self, script: FakeLlmScript) {
        self.set_scripts([script]);
    }

    pub fn set_scripts(&self, scripts: impl IntoIterator<Item = FakeLlmScript>) {
        let scripts = scripts.into_iter().collect::<Vec<_>>();
        assert!(
            !scripts.is_empty(),
            "at least one fake LLM script is required"
        );
        *self.state.scripts.lock().expect("script lock") = scripts;
    }

    /// Returns clones in request order, including the exact OpenAI wire body.
    pub fn recorded_requests(&self) -> Vec<Value> {
        self.state.requests.lock().expect("request lock").clone()
    }

    pub fn chat_request_count(&self) -> usize {
        self.state.chat_requests.load(Ordering::SeqCst)
    }

    pub fn models_request_count(&self) -> usize {
        self.state.models_requests.load(Ordering::SeqCst)
    }

    pub fn stream_disconnects(&self) -> usize {
        self.state.stream_disconnects.load(Ordering::SeqCst)
    }

    /// Waits until at least `count` chat requests have arrived.
    pub async fn wait_for_chat_requests(&self, count: usize, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, async {
            loop {
                let notified = self.state.request_arrived.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.chat_request_count() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }

    /// Waits for the server-side stream body to observe a client disconnect.
    pub async fn wait_for_stream_disconnect(&self, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, async {
            loop {
                let notified = self.state.stream_dropped.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.stream_disconnects() > 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }
}

impl Drop for FakeLlmServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
