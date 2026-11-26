//! Scripted fake Ollama server: `/api/version`, `/api/tags`, `/api/pull` and
//! `/v1/models`. It records every call so tests can prove that nothing pulled
//! a model without an explicit request.

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
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::watch};

/// One step of a streamed `/api/pull` response.
#[derive(Clone, Debug)]
pub enum FakePullStep {
    /// A JSON line followed by a newline.
    Line(Value),
    /// Raw bytes, for oversized or malformed lines.
    Raw(String),
    Delay(Duration),
    /// Sends nothing more and keeps the connection open.
    Hang,
    /// Waits until `FakeOllamaServer::release_pulls` is called.
    Gate,
}

impl FakePullStep {
    pub fn progress(status: &str) -> Self {
        Self::Line(json!({ "status": status }))
    }

    pub fn layer(digest: &str, total: u64, completed: u64) -> Self {
        Self::Line(json!({
            "status": format!("pulling {digest}"), "digest": digest,
            "total": total, "completed": completed
        }))
    }

    /// A complete successful pull of one layer.
    pub fn default_steps() -> Vec<Self> {
        FakeOllamaScript::default().pull
    }

    pub fn success() -> Self {
        Self::Line(json!({ "status": "success" }))
    }

    pub fn error(message: &str) -> Self {
        Self::Line(json!({ "error": message }))
    }
}

#[derive(Clone, Debug)]
pub struct FakeOllamaScript {
    pub models: Vec<Value>,
    pub pull: Vec<FakePullStep>,
    /// Status for `/api/pull` before streaming; `None` streams the steps.
    pub pull_status: Option<(StatusCode, String)>,
}

impl Default for FakeOllamaScript {
    fn default() -> Self {
        Self {
            models: vec![
                json!({"name": "llama3.2:3b", "model": "llama3.2:3b", "size": 2_019_393_189_u64,
                       "digest": "a80c4f17acd5", "modified_at": "2025-01-02T03:04:05Z"}),
            ],
            pull: vec![
                FakePullStep::progress("pulling manifest"),
                FakePullStep::layer("sha256:aaa", 1000, 250),
                FakePullStep::layer("sha256:aaa", 1000, 1000),
                FakePullStep::progress("verifying sha256 digest"),
                FakePullStep::success(),
            ],
            pull_status: None,
        }
    }
}

#[derive(Clone)]
struct State_ {
    script: Arc<Mutex<FakeOllamaScript>>,
    pulls: Arc<Mutex<Vec<Value>>>,
    tags: Arc<AtomicUsize>,
    version: Arc<AtomicUsize>,
    authorization: Arc<Mutex<Vec<Option<String>>>>,
    gate: watch::Sender<bool>,
}

pub struct FakeOllamaServer {
    address: SocketAddr,
    state: State_,
    task: tokio::task::JoinHandle<()>,
}

impl FakeOllamaServer {
    pub async fn start(script: FakeOllamaScript) -> Self {
        let state = State_ {
            script: Arc::new(Mutex::new(script)),
            pulls: Arc::default(),
            tags: Arc::default(),
            version: Arc::default(),
            authorization: Arc::default(),
            gate: watch::channel(false).0,
        };
        let app = Router::new()
            .route("/api/version", get(version))
            .route("/api/tags", get(tags))
            .route("/api/pull", post(pull))
            .route("/v1/models", get(openai_models))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Ollama");
        let address = listener.local_addr().expect("listener address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake Ollama");
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

    /// The OpenAI-compatible base URL as configured in `llm.base_url`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    pub fn set_script(&self, script: FakeOllamaScript) {
        *self.state.script.lock().expect("script lock") = script;
    }

    /// Bodies of every `/api/pull` request, in order.
    pub fn pull_requests(&self) -> Vec<Value> {
        self.state.pulls.lock().expect("pull lock").clone()
    }

    pub fn pull_count(&self) -> usize {
        self.pull_requests().len()
    }

    pub fn tags_count(&self) -> usize {
        self.state.tags.load(Ordering::SeqCst)
    }

    pub fn version_count(&self) -> usize {
        self.state.version.load(Ordering::SeqCst)
    }

    /// The `Authorization` header of each request, in order.
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.state
            .authorization
            .lock()
            .expect("authorization lock")
            .clone()
    }

    /// Lets every pull waiting on a `Gate` step continue.
    pub fn release_pulls(&self) {
        self.state.gate.send_replace(true);
    }
}

impl Drop for FakeOllamaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn record(state: &State_, headers: &HeaderMap) {
    state
        .authorization
        .lock()
        .expect("authorization lock")
        .push(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        );
}

async fn version(State(state): State<State_>, headers: HeaderMap) -> Response {
    record(&state, &headers);
    state.version.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "version": "0.9.0" })).into_response()
}

async fn tags(State(state): State<State_>, headers: HeaderMap) -> Response {
    record(&state, &headers);
    state.tags.fetch_add(1, Ordering::SeqCst);
    let models = state.script.lock().expect("script lock").models.clone();
    Json(json!({ "models": models })).into_response()
}

async fn openai_models(State(state): State<State_>, headers: HeaderMap) -> Response {
    record(&state, &headers);
    let models = state.script.lock().expect("script lock").models.clone();
    let data = models
        .iter()
        .filter_map(|model| model.get("name").cloned())
        .map(|id| json!({ "id": id }))
        .collect::<Vec<_>>();
    Json(json!({ "object": "list", "data": data })).into_response()
}

async fn pull(
    State(state): State<State_>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    record(&state, &headers);
    state.pulls.lock().expect("pull lock").push(body);
    let script = state.script.lock().expect("script lock").clone();
    if let Some((status, message)) = script.pull_status {
        return (status, Json(json!({ "error": message }))).into_response();
    }
    let gate = state.gate.clone();
    let body = stream::unfold(script.pull.into_iter(), move |mut steps| {
        let gate = gate.clone();
        async move {
            loop {
                match steps.next()? {
                    FakePullStep::Line(value) => {
                        return Some((
                            Ok::<_, Infallible>(Bytes::from(format!("{value}\n"))),
                            steps,
                        ))
                    }
                    FakePullStep::Raw(raw) => {
                        return Some((Ok(Bytes::from(raw)), steps));
                    }
                    FakePullStep::Delay(delay) => tokio::time::sleep(delay).await,
                    FakePullStep::Hang => std::future::pending::<()>().await,
                    FakePullStep::Gate => {
                        let _ = gate.subscribe().wait_for(|open| *open).await;
                    }
                }
            }
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(body))
        .expect("fake Ollama pull response")
}
