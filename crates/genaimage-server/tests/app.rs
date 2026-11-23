use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use genaimage_api::ProblemDetails;
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{Harness, HarnessConfig};
use genaimage_server::{build_app, AppState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpSocket};
use tower::ServiceExt;

async fn app(workspace: &TempDir, backend_url: String) -> Router {
    let config =
        HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| match key {
            "HOME" => Some(workspace.path().to_str().unwrap().to_owned()),
            "GENAIMAGE_COMFYUI_URL" => Some(backend_url.clone()),
            _ => None,
        })
        .unwrap();
    let backend = ComfyUiClient::new(&config.comfyui).unwrap();
    let harness = Harness::new_async(config, std::sync::Arc::new(backend))
        .await
        .unwrap();
    build_app(AppState::new(harness.into()))
}

async fn health_json(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn health_reports_a_reachable_fake_backend_and_echoes_request_id() {
    let fake = Router::new().route(
        "/system_stats",
        get(|| async { Json(json!({"system": {}})) }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let backend = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    let workspace = TempDir::new().unwrap();
    let response = app(&workspace, url)
        .await
        .oneshot(
            Request::get("/api/v1/health")
                .header("x-request-id", "health-check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["x-request-id"], "health-check");
    assert_eq!(
        health_json(response).await,
        json!({"self": "ok", "image_backend": "ok", "llm": "not_configured"})
    );
    backend.abort();
}

#[tokio::test]
async fn health_reports_an_unreachable_backend_and_generates_request_ids() {
    // Reserve the backend port without listening, so nothing can accept connections.
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let workspace = TempDir::new().unwrap();
    let router = app(
        &workspace,
        format!("http://{}", socket.local_addr().unwrap()),
    )
    .await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(Request::get("/api/v1/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        ids.push(response.headers()["x-request-id"].clone());
        assert_eq!(
            health_json(response).await,
            json!({"self": "ok", "image_backend": "unavailable", "llm": "not_configured"})
        );
    }
    assert!(!ids[0].is_empty());
    assert!(!ids[1].is_empty());
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn api_routing_errors_are_problem_details() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, "http://127.0.0.1:8188".into()).await;
    for (request, status, title) in [
        (
            Request::get("/api/v1/missing"),
            StatusCode::NOT_FOUND,
            "Not Found",
        ),
        (
            Request::post("/api/v1/health"),
            StatusCode::METHOD_NOT_ALLOWED,
            "Method Not Allowed",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(response.headers().contains_key("x-request-id"));
        if status == StatusCode::METHOD_NOT_ALLOWED {
            assert!(response.headers()[header::ALLOW]
                .to_str()
                .unwrap()
                .contains("GET"));
        }
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let problem: ProblemDetails = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem.problem_type, "about:blank");
        assert_eq!(problem.title, title);
        assert_eq!(problem.status, status.as_u16());
        assert!(!problem.detail.is_empty());
        assert!(!problem.recovery.is_empty());
    }
}
