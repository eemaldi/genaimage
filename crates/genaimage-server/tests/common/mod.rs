//! Isolated API fixture: `ApiHarness::start()` opens loopback listeners and a
//! temporary workspace. Script `fake`, subscribe before an action, then use
//! the JSON/request helpers and `await_event`. Dropping it stops both servers.
#![allow(dead_code)]

use std::{path::Path, sync::Arc, time::Duration};

use genaimage_api::ProblemDetails;
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    EventEnvelope, EventSubscription, Harness, HarnessConfig, RunRecord, RunRequest,
};
use genaimage_server::{build_app, AppState};
use genaimage_test_support::{FakeComfyServer, FakeComfyState};
use reqwest::{header, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};

pub struct ApiHarness {
    pub fake: FakeComfyState,
    _backend: FakeComfyServer,
    pub core: Arc<Harness>,
    server: JoinHandle<()>,
    client: reqwest::Client,
    pub origin: String,
    workspace: TempDir,
}

impl ApiHarness {
    pub async fn start() -> Self {
        Self::start_with_backend_timeout(5).await
    }

    pub async fn start_with_backend_timeout(seconds: u64) -> Self {
        let workspace = TempDir::new().expect("temporary workspace");
        let fake = FakeComfyState::default();
        let backend = FakeComfyServer::start(fake.clone()).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind API");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config_path = workspace.path().join("genaimage.toml");
        std::fs::write(&config_path, format!(
            "[server]\nport = {}\n[comfyui]\nbase_url = \"http://{}\"\nrequest_timeout_seconds = {}\nwebsocket_reconnect_seconds = 1\n[paths]\nstate_database = \"state.sqlite3\"\nworkflows = \"workflows\"\nruns = \"runs\"\nuploads = \"uploads\"\n", listener.local_addr().unwrap().port(), backend.address(), seconds
        )).unwrap();
        let root = workspace.path().to_str().unwrap().to_owned();
        let config = HarnessConfig::load_with(
            workspace.path(),
            Some(&config_path),
            workspace.path(),
            |key| match key {
                "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => Some(root.clone()),
                _ => None,
            },
        )
        .unwrap();
        let adapter = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let core = Arc::new(Harness::new_async(config, adapter).await.unwrap());
        let app = build_app(AppState::new(core.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("API server");
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        Self {
            workspace,
            fake,
            _backend: backend,
            core,
            server,
            client,
            origin,
        }
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

    /// Submit explicitly through core until the HTTP run route exists.
    pub async fn submit_run(&self, request: RunRequest) -> genaimage_core::Result<RunRecord> {
        self.core.submit(request).await
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
        self.server.abort();
    }
}
