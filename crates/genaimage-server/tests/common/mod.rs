//! Isolated API fixture: `ApiHarness::start()` opens loopback listeners and a
//! temporary workspace. Script `fake`, subscribe before an action, then use
//! the JSON/request helpers and `await_event`. Use `shutdown().await` for a
//! bounded graceful drain; dropping the fixture signals shutdown as best effort.
#![allow(dead_code)]

use std::{path::Path, sync::Arc, time::Duration};

use genaimage_api::ProblemDetails;
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    EventEnvelope, EventSubscription, Harness, HarnessConfig, RunRecord, RunRequest,
};
use genaimage_server::{serve_on_listener, AppState};
use genaimage_test_support::{
    install_contract_workflow, ApiFixture, FakeComfyServer, FakeComfyState,
};
use reqwest::{header, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use tempfile::TempDir;
use tokio::{task::JoinHandle, time::timeout};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ApiHarness {
    pub fake: FakeComfyState,
    _backend: FakeComfyServer,
    pub core: Arc<Harness>,
    server: Option<JoinHandle<()>>,
    state: AppState,
    client: reqwest::Client,
    pub origin: String,
    workspace: TempDir,
}

impl ApiHarness {
    pub async fn start() -> Self {
        Self::start_with_backend_timeout(5).await
    }

    pub async fn start_with_backend_timeout(seconds: u64) -> Self {
        Self::start_with_config(seconds, &[]).await
    }

    pub async fn start_with_config(seconds: u64, env: &[(&str, &str)]) -> Self {
        Self::start_configured(seconds, env, Duration::from_secs(2)).await
    }

    pub async fn start_with_deadlines(seconds: u64, drain: Duration) -> Self {
        Self::start_configured(seconds, &[], drain).await
    }

    async fn start_configured(seconds: u64, env: &[(&str, &str)], drain: Duration) -> Self {
        let max_upload_bytes = genaimage_core::config::ServerConfig::default().max_upload_bytes;
        let ApiFixture {
            workspace,
            fake,
            backend,
            listener,
            config_path,
            origin,
        } = ApiFixture::start(seconds, max_upload_bytes).await;
        let root = workspace.path().to_str().unwrap().to_owned();
        let config = HarnessConfig::load_with(
            workspace.path(),
            Some(&config_path),
            workspace.path(),
            |key| {
                env.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_owned())
                    .or_else(|| {
                        matches!(key, "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME")
                            .then(|| root.clone())
                    })
            },
        )
        .unwrap();
        let adapter = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let core = Arc::new(Harness::new_async(config, adapter).await.unwrap());
        let state = AppState::new(core.clone());
        let shutdown = state.clone();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            serve_on_listener(server_state, listener, shutdown.wait_for_shutdown(), drain)
                .await
                .expect("API server");
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        Self {
            workspace,
            fake,
            _backend: backend,
            core,
            server: Some(server),
            state,
            client,
            origin,
        }
    }

    /// Drain without deleting the workspace, so callers can inspect persisted state.
    pub async fn drain(&mut self) {
        self.state.begin_shutdown();
        let server = self.server.as_mut().expect("API server task");
        match timeout(SHUTDOWN_TIMEOUT, &mut *server).await {
            Ok(result) => {
                self.server = None;
                result.expect("API server task failed");
            }
            Err(_) => {
                server.abort();
                panic!("timed out draining API server");
            }
        }
    }

    /// Signal draining and wait for all HTTP connections and run tasks.
    pub async fn shutdown(mut self) {
        self.drain().await;
    }

    pub fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    /// The URL supplies Host; non-GET/HEAD requests use a same-origin Origin.
    pub fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let origin = if method != Method::GET && method != Method::HEAD {
            Some(self.origin.as_str())
        } else {
            None
        };
        self.request_with_origin(method, path, origin)
    }

    /// Use a specific Origin, or omit it, for request-hardening tests.
    pub fn request_with_origin(
        &self,
        method: Method,
        path: &str,
        origin: Option<&str>,
    ) -> reqwest::RequestBuilder {
        assert!(path.starts_with('/') && !path.starts_with("//"));
        let request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        match origin {
            Some(origin) => request.header(header::ORIGIN, origin),
            None => request,
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> T {
        let response = self.request(Method::GET, path).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    pub async fn post_json<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> T {
        let response = self
            .request(Method::POST, path)
            .json(body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "unexpected status {}",
            response.status()
        );
        response.json().await.unwrap()
    }

    pub async fn assert_problem(response: Response, status: StatusCode) -> ProblemDetails {
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let problem: ProblemDetails = response.json().await.unwrap();
        assert_eq!(problem.problem_type, "about:blank");
        assert_eq!(problem.status, status.as_u16());
        assert!(!problem.detail.is_empty());
        assert!(!problem.recovery.is_empty());
        problem
    }

    /// Submit through the core for tests that exercise the fixture independently of the HTTP routes.
    pub async fn submit_run(&self, request: RunRequest) -> genaimage_core::Result<RunRecord> {
        self.state.harness().submit(request).await
    }

    pub fn subscribe(&self) -> EventSubscription {
        self.core.events().subscribe()
    }

    /// Subscribe before the triggering action; fail on timeout or lost events.
    pub async fn await_event(
        subscription: &mut EventSubscription,
        deadline: Duration,
        matches: impl Fn(&EventEnvelope) -> bool,
    ) -> EventEnvelope {
        timeout(deadline, async {
            loop {
                let event = subscription.recv().await.expect("event subscription lost");
                if matches(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }
}

impl Drop for ApiHarness {
    fn drop(&mut self) {
        self.state.begin_shutdown();
        if let Some(mut server) = self.server.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if timeout(SHUTDOWN_TIMEOUT, &mut server).await.is_err() {
                        server.abort();
                    }
                });
            } else {
                server.abort();
            }
        }
    }
}

pub fn install_workflow(api: &ApiHarness) {
    install_contract_workflow(&api.core.config().workflows);
}
