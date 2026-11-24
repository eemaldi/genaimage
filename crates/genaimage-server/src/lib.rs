//! The loopback HTTP adapter for GenAImage.

mod listener;
mod portal;
mod problem;
mod routes;
mod security;
mod settings;
mod workflow_index;

use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderName, Request},
    middleware, Router,
};
use genaimage_core::{
    assistant::{ConversationService, PromptTemplates},
    Harness, HarnessError, LlmRoles,
};
use tokio::task::JoinHandle;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;
use workflow_index::WorkflowIndexCache;

pub use listener::{serve, serve_on_listener};
pub use portal::PortalDevProxy;
pub use problem::Problem;
pub use routes::openapi;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    settings: Arc<settings::SettingsState>,
    portal_dev: Option<PortalDevProxy>,
    workflow_index: Arc<WorkflowIndexCache>,
    llm_roles: LlmRoles,
    conversation_service: ConversationService,
    // Shared by tasks from every active-harness generation.
    lifecycle: Arc<Lifecycle>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("settings", &self.settings)
            .field("llm_roles", &self.llm_roles)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Lifecycle {
    admission: Mutex<()>,
    shutdown: CancellationToken,
    deadline: CancellationToken,
    tasks: TaskTracker,
    monitors: Mutex<HashSet<String>>,
}

struct MonitorRegistration(Arc<Lifecycle>, String);

impl Drop for MonitorRegistration {
    fn drop(&mut self) {
        self.0
            .monitors
            .lock()
            .expect("monitor lock")
            .remove(&self.1);
    }
}

impl AppState {
    pub fn new(harness: Arc<Harness>) -> Self {
        let llm_roles = LlmRoles::new(&harness.config().llm, None);
        let conversation_service = ConversationService::unbound(
            harness.conversation_store(),
            PromptTemplates::new(harness.config().data_dir()),
        );
        Self {
            settings: Arc::new(settings::SettingsState::new(harness)),
            lifecycle: Arc::default(),
            portal_dev: None,
            workflow_index: Arc::default(),
            llm_roles,
            conversation_service,
        }
    }

    pub fn with_llm_roles(mut self, llm_roles: LlmRoles) -> Self {
        self.llm_roles = llm_roles;
        self
    }

    pub fn with_assistant_tools(
        mut self,
        tools: Vec<Arc<dyn genaimage_core::AssistantTool>>,
    ) -> Result<Self, HarnessError> {
        self.conversation_service = self.conversation_service.with_assistant_tools(tools)?;
        Ok(self)
    }

    /// Serve the portal through Vite instead of using embedded assets.
    pub fn with_portal_dev(mut self, proxy: PortalDevProxy) -> Self {
        self.portal_dev = Some(proxy);
        self
    }

    /// Snapshot the active harness once per request or background operation.
    pub fn harness(&self) -> Arc<Harness> {
        self.settings.harness()
    }

    pub fn llm_roles(&self) -> &LlmRoles {
        &self.llm_roles
    }

    pub(crate) fn conversation_service(&self) -> ConversationService {
        self.conversation_service.clone()
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

    fn ensure_run_monitor(&self, harness: Arc<Harness>, id: String) {
        if !self
            .lifecycle
            .monitors
            .lock()
            .expect("monitor lock")
            .insert(id.clone())
        {
            return;
        }
        let registration = MonitorRegistration(self.lifecycle.clone(), id);
        self.spawn_run_task(async move {
            let MonitorRegistration(lifecycle, id) = &registration;
            if let Err(error) = harness.monitor_run(id, &lifecycle.shutdown, &lifecycle.deadline).await {
                tracing::warn!(run_id = id, %error, "Run monitoring stopped; reconcile to refresh the state");
            }
        });
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
        .fallback(portal::fallback)
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
