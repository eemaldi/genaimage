//! The loopback HTTP adapter for GenAImage.

mod listener;
mod problem;
mod routes;
mod security;

use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderName, Request},
    middleware, Router,
};
use genaimage_core::Harness;
use tokio::sync::watch;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

pub use listener::serve;
pub use problem::Problem;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    harness: Arc<Harness>,
    shutdown: watch::Sender<bool>,
}

impl AppState {
    pub fn new(harness: Arc<Harness>) -> Self {
        Self {
            harness,
            shutdown: watch::Sender::new(false),
        }
    }

    /// Resolves when graceful draining starts, even if it has already started.
    /// Long-lived response streams (such as SSE) should await this to close
    /// without holding up server shutdown.
    pub async fn wait_for_shutdown(&self) {
        let _ = self
            .shutdown
            .subscribe()
            .wait_for(|draining| *draining)
            .await;
    }
}

/// Builds the application without opening a socket or installing signal handlers.
/// `port` must be the actual listener port (or the intended port for in-process callers).
pub fn build_app(state: AppState, port: u16) -> Router {
    let security = security::SecurityConfig {
        port,
        bind_host: state.harness.config().server.host.parse().ok(),
        app_state: state.clone(),
        max_upload_bytes: state.harness.config().server.max_upload_bytes,
    };
    Router::new()
        .nest("/api/v1", routes::api())
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
