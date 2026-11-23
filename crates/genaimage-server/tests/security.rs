use std::{convert::Infallible, sync::Arc};

use axum::{
    body::{to_bytes, Body, Bytes},
    http::{header, Request, StatusCode},
    response::Response,
    Router,
};
use futures_util::stream;
use genaimage_api::ProblemDetails;
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{Harness, HarnessConfig};
use genaimage_server::{build_app, AppState};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn app(workspace: &TempDir, port: u16) -> Router {
    let config =
        HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| match key {
            "HOME" => Some(workspace.path().to_str().unwrap().to_owned()),
            "GENAIMAGE_SERVER_MAX_UPLOAD_BYTES" => Some("16".to_owned()),
            _ => None,
        })
        .unwrap();
    let backend = ComfyUiClient::new(&config.comfyui).unwrap();
    let harness = Harness::new_async(config, Arc::new(backend)).await.unwrap();
    build_app(AppState::new(harness.into()), port)
}

async fn problem(response: Response, status: StatusCode) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert!(response.headers().contains_key("x-request-id"));
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert!(response.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert!(!response
        .headers()
        .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let details: ProblemDetails = serde_json::from_slice(&body).unwrap();
    assert_eq!(details.status, status.as_u16());
    assert!(!details.recovery.is_empty());
}

#[tokio::test]
async fn hosts_and_origins_are_checked_together() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    for host in [
        "127.0.0.1:4567",
        "localhost:4567",
        "[::1]:4567",
        "LOCALHOST:4567",
    ] {
        for (origin, fetch_site) in [
            (None, None),
            (Some(format!("http://{host}")), None),
            (None, Some("same-origin")),
        ] {
            let mut request = Request::post("/api/v1/health").header(header::HOST, host);
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            if let Some(fetch_site) = fetch_site {
                request = request.header("sec-fetch-site", fetch_site);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
        }
    }
    for host in [
        "evil.example:4567",
        "127.0.0.1:4568",
        "localhost",
        "127.0.0.1.evil.example:4567",
        "localhost:4567@evil.example",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/v1/health")
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        problem(response, StatusCode::MISDIRECTED_REQUEST).await;
    }
    for request in [
        Request::get("/api/v1/health"),
        Request::get("/api/v1/health")
            .header(header::HOST, "localhost:4567")
            .header(header::HOST, "evil.example:4567"),
        Request::get("http://evil.example:4567/api/v1/health")
            .header(header::HOST, "localhost:4567"),
    ] {
        problem(
            router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap(),
            StatusCode::MISDIRECTED_REQUEST,
        )
        .await;
    }
    for request in [
        Request::post("/api/v1/health").header(header::ORIGIN, "http://evil.example"),
        Request::post("/api/v1/health").header(header::ORIGIN, "http://localhost:4568"),
        Request::post("/api/v1/health").header(header::ORIGIN, "http://127.0.0.1:4567"),
        Request::post("/api/v1/health").header(header::ORIGIN, "null"),
        Request::post("/api/v1/health")
            .header(header::ORIGIN, "http://localhost:4567")
            .header(header::ORIGIN, "http://evil.example"),
        Request::post("/api/v1/health").header("sec-fetch-mode", "cors"),
        Request::post("/api/v1/health").header(header::USER_AGENT, "Mozilla/5.0"),
        Request::post("/api/v1/health").header("sec-fetch-site", "cross-site"),
        Request::post("/api/v1/health").header("sec-fetch-site", "same-site"),
        Request::post("/api/v1/health").header("sec-fetch-site", "none"),
        Request::post("/api/v1/health")
            .header(header::ORIGIN, "http://localhost:4567")
            .header("sec-fetch-site", "cross-site"),
        Request::post("/api/v1/health")
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-site", "cross-site"),
    ] {
        let response = router
            .clone()
            .oneshot(
                request
                    .header(header::HOST, "localhost:4567")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        problem(response, StatusCode::FORBIDDEN).await;
    }
}

#[tokio::test]
async fn bodies_are_limited_even_without_a_length_header() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    for (path, size, status) in [
        (
            "/api/v1/health",
            1024 * 1024,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "/api/v1/health",
            1024 * 1024 + 1,
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        ("/api/v1/uploads", 16, StatusCode::NOT_FOUND),
        ("/api/v1/uploads", 17, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let chunks = stream::iter(vec![Ok::<_, Infallible>(Bytes::from(vec![b'x'; size]))]);
        let request = Request::post(path)
            .header(header::HOST, "127.0.0.1:4567")
            .body(Body::from_stream(chunks))
            .unwrap();
        problem(router.clone().oneshot(request).await.unwrap(), status).await;
    }
    let chunks = stream::iter(vec![
        Ok::<_, Infallible>(Bytes::from_static(b"12345678")),
        Ok(Bytes::from_static(b"123456789")),
    ]);
    let request = Request::post("/api/v1/uploads")
        .header(header::HOST, "127.0.0.1:4567")
        .body(Body::from_stream(chunks))
        .unwrap();
    problem(
        router.clone().oneshot(request).await.unwrap(),
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;

    let broken = stream::iter(vec![Err::<Bytes, _>(std::io::Error::other(
        "connection lost",
    ))]);
    let request = Request::post("/api/v1/uploads")
        .header(header::HOST, "127.0.0.1:4567")
        .body(Body::from_stream(broken))
        .unwrap();
    problem(
        router.oneshot(request).await.unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
}

#[tokio::test]
async fn ephemeral_listener_accepts_its_actual_port() {
    let workspace = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(&workspace, addr.port()).await;
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket.write_all(format!("POST /api/v1/health HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    server.abort();
    let _ = server.await;
}
