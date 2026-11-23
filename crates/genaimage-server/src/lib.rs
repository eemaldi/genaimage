//! The loopback HTTP adapter for GenAImage.

mod listener;
mod problem;
mod routes;
mod security;
mod settings;

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderName, Request},
    middleware, Router,
};
use genaimage_core::Harness;
use tokio::task::JoinHandle;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

pub use listener::{serve, serve_on_listener};
pub use problem::Problem;
pub use routes::openapi;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    settings: Arc<settings::SettingsState>,
    // Shared by tasks from every active-harness generation.
    lifecycle: Arc<Lifecycle>,
}

#[derive(Default)]
struct Lifecycle {
    admission: Mutex<()>,
    shutdown: CancellationToken,
    deadline: CancellationToken,
    tasks: TaskTracker,
}

impl AppState {
    pub fn new(harness: Arc<Harness>) -> Self {
        Self {
            settings: Arc::new(settings::SettingsState::new(harness)),
            lifecycle: Arc::default(),
        }
    }

    /// Snapshot the active harness once per request or background operation.
    pub fn harness(&self) -> Arc<Harness> {
        self.settings.harness()
    }

    /// Starts graceful draining and notifies long-lived handlers. Idempotent.
    pub fn begin_shutdown(&self) {
        let _admission = self
            .lifecycle
            .admission
            .lock()
            .expect("task admission lock");
        self.lifecycle.shutdown.cancel();
        self.lifecycle.tasks.close();
    }

    /// Resolves when graceful draining starts, even if it has already started.
    /// Long-lived response streams (such as SSE) should await this to close
    /// without holding up server shutdown.
    pub async fn wait_for_shutdown(&self) {
        self.lifecycle.shutdown.cancelled().await;
    }

    fn spawn_run_task<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Option<JoinHandle<T>> {
        let _admission = self
            .lifecycle
            .admission
            .lock()
            .expect("task admission lock");
        if self.lifecycle.shutdown.is_cancelled() {
            return None;
        }
        Some(self.lifecycle.tasks.spawn(future))
    }

    async fn drain_run_tasks(&self, timeout: Duration) {
        if tokio::time::timeout(timeout, self.lifecycle.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("Run task drain timed out; recording unconfirmed submissions");
            self.lifecycle.deadline.cancel();
            // Do not abort the SQLite finalisation: the reservation must reflect
            // the unknown outcome before the server returns.
            self.lifecycle.tasks.wait().await;
        }
    }
}

/// Builds the application without opening a socket or installing signal handlers.
/// `port` must be the actual listener port (or the intended port for in-process callers).
pub fn build_app(state: AppState, port: u16) -> Router {
    let harness = state.harness();
    let security = security::SecurityConfig {
        port,
        bind_host: harness.config().server.host.parse().ok(),
        app_state: state.clone(),
        max_upload_bytes: harness.config().server.max_upload_bytes,
    };
    let (router, _) = routes::router_and_openapi();
    router
        .with_state(state)
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(security, security::protect))
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    tracing::info_span!(
                        "http.request",
                        method = %request.method(),
                        path = request.uri().path(),
                        request_id = ?request.headers().get(REQUEST_ID),
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
        .layer(middleware::from_fn(security::response_headers))
}
