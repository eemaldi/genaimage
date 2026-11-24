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
    app_with_host(workspace, port, "127.0.0.1").await
}

async fn app_with_host(workspace: &TempDir, port: u16, host: &str) -> Router {
    build_app(test_state(workspace, host).await, port)
}

async fn test_state(workspace: &TempDir, host: &str) -> AppState {
    let config =
        HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| match key {
            "HOME" => Some(workspace.path().to_str().unwrap().to_owned()),
            "GENAIMAGE_SERVER_MAX_UPLOAD_BYTES" => Some("16".to_owned()),
            "GENAIMAGE_SERVER_HOST" => Some(host.to_owned()),
            _ => None,
        })
        .unwrap();
    let backend = ComfyUiClient::new(&config.comfyui).unwrap();
    let harness = Harness::new_async(config, Arc::new(backend)).await.unwrap();
    AppState::new(harness.into())
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
        ("/api/v1/uploads", 16, StatusCode::BAD_REQUEST),
        ("/api/v1/uploads", 17, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let chunks = stream::iter(vec![Ok::<_, Infallible>(Bytes::from(vec![b'x'; size]))]);
        let request = Request::post(path)
            .header(header::HOST, "127.0.0.1:4567")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=image")
            .body(Body::from_stream(chunks))
            .unwrap();
        problem(router.clone().oneshot(request).await.unwrap(), status).await;
    }
    let broken = stream::iter(vec![Err::<Bytes, _>(std::io::Error::other(
        "connection lost",
    ))]);
    let request = Request::post("/api/v1/uploads")
        .header(header::HOST, "127.0.0.1:4567")
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=image")
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

#[tokio::test]
async fn only_the_configured_additional_loopback_address_is_allowed() {
    let workspace = TempDir::new().unwrap();
    let router = app_with_host(&workspace, 4567, "127.0.0.5").await;
    for (host, status) in [
        ("127.0.0.5:4567", StatusCode::METHOD_NOT_ALLOWED),
        ("127.0.0.1:4567", StatusCode::METHOD_NOT_ALLOWED),
        ("127.0.0.6:4567", StatusCode::MISDIRECTED_REQUEST),
        ("127.0.0.5:4568", StatusCode::MISDIRECTED_REQUEST),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/v1/health")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, format!("http://{host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        problem(response, status).await;
    }
    let listener = TcpListener::bind("127.0.0.5:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let network_router = app_with_host(&workspace, addr.port(), "127.0.0.5").await;
    let server = tokio::spawn(async move { axum::serve(listener, network_router).await.unwrap() });
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(format!("POST /api/v1/health HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    server.abort();
    let _ = server.await;

    let workspace = TempDir::new().unwrap();
    let router = app_with_host(&workspace, 4567, "::ffff:127.0.0.5").await;
    let response = router
        .oneshot(
            Request::post("/api/v1/health")
                .header(header::HOST, "[::ffff:7f00:5]:4567")
                .header(header::ORIGIN, "http://[::ffff:127.0.0.5]:4567")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
}

#[tokio::test]
async fn default_http_port_allows_both_canonical_and_explicit_authorities() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 80).await;
    for (host, origin) in [
        ("localhost", "http://localhost"),
        ("localhost:80", "http://localhost"),
        ("127.0.0.1", "http://127.0.0.1:80"),
        ("[::1]", "http://[::1]"),
        ("[::1]:80", "http://[0:0:0:0:0:0:0:1]"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/v1/health")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
    }
    let configured = app_with_host(&workspace, 80, "127.0.0.5").await;
    let response = configured
        .oneshot(
            Request::post("/api/v1/health")
                .header(header::HOST, "127.0.0.5")
                .header(header::ORIGIN, "http://127.0.0.5:80")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
    for (host, origin, status) in [
        (
            "localhost:81",
            "http://localhost",
            StatusCode::MISDIRECTED_REQUEST,
        ),
        ("localhost", "http://localhost:81", StatusCode::FORBIDDEN),
        ("localhost", "http://127.0.0.1", StatusCode::FORBIDDEN),
        ("localhost", "http://localhost/", StatusCode::FORBIDDEN),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/v1/health")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        problem(response, status).await;
    }
}

async fn portal_request(router: &Router, method: &str, path: &str, encoding: &str) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:4567")
                .header(header::ACCEPT_ENCODING, encoding)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn portal_fallback_does_not_handle_api_paths() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    for path in ["/api", "/api/", "/api/v2/unknown", "/api/v1/unknown"] {
        problem(
            portal_request(&router, "GET", path, "br").await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
}

#[cfg(not(feature = "portal"))]
#[tokio::test]
async fn api_only_build_has_no_index() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    assert_eq!(
        portal_request(&router, "GET", "/", "br").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[cfg(feature = "portal")]
#[tokio::test]
async fn embedded_portal_serves_index_spa_fallback_and_immutable_assets() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    let mut pages = Vec::new();
    for path in ["/", "/unknown/deep-link", "/index.html.br"] {
        let response = portal_request(&router, "GET", path, "").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html");
        assert!(!response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("unsafe-inline"));
        pages.push(to_bytes(response.into_body(), 64 * 1024).await.unwrap());
    }
    assert_eq!(pages[0], pages[1]);
    assert_eq!(pages[0], pages[2]);
    let index = std::str::from_utf8(&pages[0]).unwrap();
    assert!(index.contains("<title>GenAImage</title>"));
    let (_, asset) = index
        .split_once("src=\"/assets/")
        .expect("Vite entry script");
    let asset = format!("/assets/{}", asset.split('"').next().unwrap());
    let response = portal_request(&router, "HEAD", &asset, "br").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
    assert!(response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .contains("javascript"));
    assert!(
        response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 0
    );
    assert!(to_bytes(response.into_body(), 0).await.unwrap().is_empty());
    assert_eq!(
        portal_request(&router, "POST", "/", "").await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[cfg(feature = "portal")]
#[tokio::test]
async fn embedded_portal_negotiates_precompressed_representations() {
    let workspace = TempDir::new().unwrap();
    let router = app(&workspace, 4567).await;
    for (accept, expected) in [
        ("gzip, br", Some("br")),
        ("gzip;q=0.8, br;q=0.4", Some("gzip")),
        ("br;q=0, *;q=0.5", Some("gzip")),
        ("br;q=0, gzip;q=0", None),
        ("br;q=0.1, identity;q=1", None),
        ("", None),
    ] {
        let response = portal_request(&router, "GET", "/", accept).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|value| value.to_str().unwrap()),
            expected
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let expected_bytes: &[u8] = match expected {
            Some("br") => include_bytes!("../../../apps/portal/dist/index.html.br"),
            Some("gzip") => include_bytes!("../../../apps/portal/dist/index.html.gz"),
            _ => include_bytes!("../../../apps/portal/dist/index.html"),
        };
        assert_eq!(bytes, expected_bytes);
    }
    let response = portal_request(&router, "GET", "/", "*;q=0").await;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
}

#[test]
fn portal_dev_url_accepts_only_loopback_http_origins() {
    use genaimage_server::PortalDevProxy;

    for url in [
        "http://192.0.2.1:5173",
        "http://localhost.example:5173",
        "http://[::ffff:192.0.2.1]:5173",
        "https://localhost:5173",
        "http://user@127.0.0.1:5173",
        "http://localhost:5173/path",
    ] {
        assert!(
            PortalDevProxy::new(url)
                .unwrap_err()
                .to_string()
                .contains("--portal-dev-url"),
            "{url}"
        );
    }
    for url in [
        "http://127.0.0.1:5173",
        "http://localhost:5173",
        "http://[::1]:5173",
    ] {
        assert!(PortalDevProxy::new(url).is_ok(), "{url}");
    }
}

#[tokio::test]
async fn portal_proxy_serves_vite_without_forwarding_api_or_browser_credentials() {
    use axum::routing::get;
    use genaimage_server::PortalDevProxy;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let vite = Router::new().fallback(get(move |request: axum::extract::Request| async move {
        let headers = request.headers();
        assert_eq!(headers[header::HOST], format!("localhost:{}", addr.port()));
        assert!(!headers.contains_key(header::COOKIE));
        (
            [
                (header::CONTENT_TYPE, "text/javascript"),
                (header::SET_COOKIE, "unexpected=value"),
            ],
            request.uri().to_string(),
        )
    }));
    let server = tokio::spawn(async move { axum::serve(listener, vite).await.unwrap() });
    let workspace = TempDir::new().unwrap();
    let state = test_state(&workspace, "127.0.0.1").await;
    let router = build_app(
        state.with_portal_dev(
            PortalDevProxy::new(&format!("http://localhost:{}", addr.port())).unwrap(),
        ),
        4567,
    );
    let response = router
        .clone()
        .oneshot(
            Request::get("/src/main.ts?t=123")
                .header(header::HOST, "127.0.0.1:4567")
                .header(header::COOKIE, "browser=value")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/javascript");
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "/src/main.ts?t=123"
    );
    problem(
        portal_request(&router, "GET", "/api/missing", "").await,
        StatusCode::NOT_FOUND,
    )
    .await;
    problem(
        router
            .oneshot(
                Request::get("/")
                    .header(header::HOST, "example.com:4567")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::MISDIRECTED_REQUEST,
    )
    .await;
    server.abort();
}
