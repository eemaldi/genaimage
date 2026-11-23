//! Scriptable fake ComfyUI wire server, shared by integration tests only.
//! Each server has an isolated state. Set the atomic switches to control health,
//! submission, queue and completion; override queue/history JSON for more complex
//! scenarios. Subscribe to progress before sending JSON on `progress`.
//! The default queue and history represent the most recently submitted prompt.

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocketUpgrade},
        Path as AxumPath, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch, Notify},
};

#[derive(Clone)]
pub struct FakeComfyState {
    pub prompt_id: Arc<Mutex<Option<String>>>,
    pub complete: Arc<AtomicBool>,
    pub cancelled: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
    pub fail_submit_response: Arc<AtomicBool>,
    pub hang_submit_response: Arc<AtomicBool>,
    pub redirect_submit_response: Arc<AtomicBool>,
    pub prompt_calls: Arc<AtomicUsize>,
    pub upload_calls: Arc<AtomicUsize>,
    pub interrupt_calls: Arc<AtomicUsize>,
    pub health_unavailable: Arc<AtomicBool>,
    pub health_hangs: Arc<AtomicBool>,
    pub missing_classes: Arc<Mutex<Vec<String>>>,
    pub object_info_unavailable: Arc<AtomicBool>,
    pub object_info_hangs: Arc<AtomicBool>,
    pub object_info_override: Arc<Mutex<Option<Value>>>,
    pub queue_override: Arc<Mutex<Option<Value>>>,
    pub history_overrides: Arc<Mutex<BTreeMap<String, Value>>>,
    pub progress: broadcast::Sender<Value>,
    connected: Arc<Notify>,
    shutdown: watch::Sender<bool>,
}

impl Default for FakeComfyState {
    fn default() -> Self {
        Self {
            prompt_id: Default::default(),
            complete: Default::default(),
            cancelled: Default::default(),
            running: Default::default(),
            fail_submit_response: Default::default(),
            hang_submit_response: Default::default(),
            redirect_submit_response: Default::default(),
            prompt_calls: Default::default(),
            upload_calls: Default::default(),
            interrupt_calls: Default::default(),
            health_unavailable: Default::default(),
            health_hangs: Default::default(),
            missing_classes: Default::default(),
            object_info_unavailable: Default::default(),
            object_info_hangs: Default::default(),
            object_info_override: Default::default(),
            queue_override: Default::default(),
            history_overrides: Default::default(),
            progress: broadcast::channel(32).0,
            connected: Arc::new(Notify::new()),
            shutdown: watch::channel(false).0,
        }
    }
}

impl FakeComfyState {
    /// Wait up to `deadline` for a WebSocket subscriber before emitting progress.
    pub async fn wait_for_progress_listener(
        &self,
        deadline: Duration,
    ) -> Result<(), tokio::time::error::Elapsed> {
        tokio::time::timeout(deadline, async {
            loop {
                let notified = self.connected.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.progress.receiver_count() > 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
    }
}

async fn fake_websocket(State(state): State<FakeComfyState>, ws: WebSocketUpgrade) -> Response {
    let mut rx = state.progress.subscribe();
    state.connected.notify_waiters();
    let mut shutdown = state.shutdown.subscribe();
    ws.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                _ = async { let _ = shutdown.wait_for(|closed| *closed).await; } => break,
                message = rx.recv() => match message {
                    Ok(value) => if socket.send(Message::Text(value.to_string().into())).await.is_err() { break; },
                    Err(_) => break,
                },
                incoming = socket.recv() => if !matches!(incoming, Some(Ok(_))) { break; },
            }
        }
    })
}

async fn fake_system(State(state): State<FakeComfyState>) -> Response {
    if state.health_hangs.load(Ordering::SeqCst) {
        let _ = state.shutdown.subscribe().wait_for(|closed| *closed).await;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if state.health_unavailable.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({ "system": { "os": "test" }, "devices": [] })).into_response()
}

async fn fake_object_info(State(state): State<FakeComfyState>) -> Response {
    if state.object_info_hangs.load(Ordering::SeqCst) {
        let _ = state.shutdown.subscribe().wait_for(|closed| *closed).await;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if state.object_info_unavailable.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(value) = state
        .object_info_override
        .lock()
        .expect("object info lock")
        .clone()
    {
        return Json(value).into_response();
    }
    let mut info = json!({
        "CheckpointLoaderSimple": {},
        "CLIPTextEncode": {},
        "EmptyLatentImage": {},
        "VAEDecode": {},
        "KSampler": {
            "input": {
                "required": {
                    "seed": ["INT", {}],
                    "steps": ["INT", {}]
                }
            }
        },
        "SaveImage": {
            "input": {
                "required": {
                    "filename_prefix": ["STRING", {}]
                }
            }
        },
        "LoadImage": {
            "input": {
                "required": {
                    "image": ["STRING", {}]
                }
            }
        }
    });
    for class in state.missing_classes.lock().expect("class lock").iter() {
        info.as_object_mut().expect("object info").remove(class);
    }
    Json(info).into_response()
}

async fn fake_upload(State(state): State<FakeComfyState>, _body: Bytes) -> Json<Value> {
    state.upload_calls.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "name": "uploaded-reference.png",
        "subfolder": "",
        "type": "input"
    }))
}

async fn fake_prompt(State(state): State<FakeComfyState>, Json(body): Json<Value>) -> Response {
    state.prompt_calls.fetch_add(1, Ordering::SeqCst);
    let prompt_id = body["prompt_id"].as_str().expect("prompt ID").to_owned();
    *state.prompt_id.lock().expect("prompt lock") = Some(prompt_id.clone());
    if state.hang_submit_response.load(Ordering::SeqCst) {
        let _ = state.shutdown.subscribe().wait_for(|closed| *closed).await;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if state.redirect_submit_response.load(Ordering::SeqCst) {
        return Redirect::temporary("/prompt").into_response();
    }
    if state.fail_submit_response.load(Ordering::SeqCst) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "deliberate fake failure" })),
        )
            .into_response();
    }
    Json(json!({
        "prompt_id": prompt_id,
        "number": 1,
        "node_errors": {}
    }))
    .into_response()
}

async fn fake_queue(State(state): State<FakeComfyState>) -> Json<Value> {
    if let Some(queue) = state.queue_override.lock().expect("queue lock").clone() {
        return Json(queue);
    }
    let prompt_id = state.prompt_id.lock().expect("prompt lock").clone();
    let has_job = !state.complete.load(Ordering::SeqCst)
        && !state.cancelled.load(Ordering::SeqCst)
        && prompt_id.is_some();
    let entry = has_job.then(|| json!([1, prompt_id, {}, {}]));
    let running = if has_job && state.running.load(Ordering::SeqCst) {
        entry.clone().into_iter().collect()
    } else {
        Vec::new()
    };
    let pending = if has_job && !state.running.load(Ordering::SeqCst) {
        entry.into_iter().collect()
    } else {
        Vec::new()
    };
    Json(json!({
        "queue_running": running,
        "queue_pending": pending
    }))
}

async fn fake_interrupt(State(state): State<FakeComfyState>) -> Json<Value> {
    state.interrupt_calls.fetch_add(1, Ordering::SeqCst);
    state.cancelled.store(true, Ordering::SeqCst);
    Json(json!({}))
}

async fn fake_queue_delete(
    State(state): State<FakeComfyState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let prompt_id = state.prompt_id.lock().expect("prompt lock").clone();
    if prompt_id.as_deref().is_some_and(|prompt_id| {
        body["delete"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(prompt_id)))
    }) {
        state.cancelled.store(true, Ordering::SeqCst);
    }
    Json(json!({}))
}

async fn fake_history(
    State(state): State<FakeComfyState>,
    AxumPath(prompt_id): AxumPath<String>,
) -> Result<Json<Value>, StatusCode> {
    if let Some(history) = state
        .history_overrides
        .lock()
        .expect("history lock")
        .get(&prompt_id)
        .cloned()
    {
        return Ok(Json(history));
    }
    if state.prompt_id.lock().expect("prompt lock").as_deref() != Some(prompt_id.as_str()) {
        return Ok(Json(json!({})));
    }
    if state.interrupt_calls.load(Ordering::SeqCst) > 0 {
        return Ok(Json(json!({
            prompt_id.clone(): {
                "status": {
                    "status_str": "error",
                    "completed": false,
                    "messages": [["execution_interrupted", {"prompt_id": prompt_id}]]
                }
            }
        })));
    }
    if !state.complete.load(Ordering::SeqCst) {
        return Ok(Json(json!({})));
    }
    Ok(Json(json!({
        prompt_id: {
            "status": {
                "status_str": "success",
                "completed": true,
                "messages": []
            },
            "outputs": {
                "9": {
                    "images": [{
                        "filename": "ComfyUI_00001_.png",
                        "subfolder": "",
                        "type": "output"
                    }]
                }
            }
        }
    })))
}

async fn fake_view(Query(query): Query<BTreeMap<String, String>>) -> Result<Vec<u8>, StatusCode> {
    if !query.contains_key("filename") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(b"not-a-real-png-but-deterministic".to_vec())
}

/// Owns the listener and aborts its task on drop.
pub struct FakeComfyServer {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
}

impl FakeComfyServer {
    pub async fn start(state: FakeComfyState) -> Self {
        let shutdown = state.shutdown.clone();
        let app = Router::new()
            .route("/system_stats", get(fake_system))
            .route("/object_info", get(fake_object_info))
            .route("/upload/image", post(fake_upload))
            .route("/prompt", post(fake_prompt))
            .route("/interrupt", post(fake_interrupt))
            .route("/queue", get(fake_queue).post(fake_queue_delete))
            .route("/history/{prompt_id}", get(fake_history))
            .route("/view", get(fake_view))
            .route("/ws", get(fake_websocket))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake ComfyUI");
        let address = listener.local_addr().expect("listener address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake server");
        });
        Self {
            address,
            task,
            shutdown,
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for FakeComfyServer {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn progress_listener_wait_times_out_if_no_client_connects() {
        let state = FakeComfyState::default();
        assert!(state
            .wait_for_progress_listener(Duration::from_millis(20))
            .await
            .is_err());
    }
}
