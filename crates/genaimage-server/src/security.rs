use std::{error::Error as _, net::IpAddr, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{AppState, Problem};

const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct SecurityConfig {
    pub port: u16,
    pub bind_host: Option<IpAddr>,
    pub app_state: AppState,
    pub max_upload_bytes: usize,
}

// Keep this routing decision alongside new endpoints, before their handlers read a body.
fn body_limit(path: &str, config: &SecurityConfig) -> usize {
    match path {
        "/api/v1/uploads" => config.max_upload_bytes,
        _ => DEFAULT_BODY_LIMIT,
    }
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|value| value.to_str().map_err(|_| ()))
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Host {
    Localhost,
    Ip(IpAddr),
}

fn parse_host(authority: &str, port: u16) -> Option<Host> {
    let suffix = format!(":{port}");
    let name = match authority.strip_suffix(&suffix) {
        Some(name) => name,
        None if port == 80 => authority,
        None => return None,
    };
    if name.eq_ignore_ascii_case("localhost") {
        return Some(Host::Localhost);
    }
    let address = if name.starts_with('[') {
        name.strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .ok()?
            .into()
    } else {
        name.parse::<std::net::Ipv4Addr>().ok()?.into()
    };
    Some(Host::Ip(address))
}

fn allowed_host(authority: &str, config: &SecurityConfig) -> Option<Host> {
    let host = parse_host(authority, config.port)?;
    match host {
        Host::Localhost => Some(host),
        Host::Ip(ip)
            if ip == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                || ip == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
                || (config.bind_host == Some(ip) && ip.to_canonical().is_loopback()) =>
        {
            Some(host)
        }
        Host::Ip(_) => None,
    }
}

pub(crate) async fn protect(
    State(config): State<SecurityConfig>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let host = match single_header(request.headers(), "host")
        .ok()
        .flatten()
        .and_then(|host| allowed_host(host, &config))
    {
        Some(host) => host,
        None => {
            return Problem::new(
                StatusCode::MISDIRECTED_REQUEST,
                "The Host header is not allowed.",
                "Use a loopback host and the server's actual port.",
            )
            .into_response()
        }
    };
    if request
        .uri()
        .authority()
        .is_some_and(|authority| parse_host(authority.as_str(), config.port) != Some(host))
    {
        return Problem::new(
            StatusCode::MISDIRECTED_REQUEST,
            "The request authority does not match Host.",
            "Use the server's loopback address and port.",
        )
        .into_response();
    }
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        let origin = single_header(request.headers(), "origin");
        let fetch_site = single_header(request.headers(), "sec-fetch-site");
        let valid_origin = match origin {
            Ok(None) => true,
            Ok(Some(value)) => {
                value
                    .get(..7)
                    .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
                    .and_then(|_| parse_host(&value[7..], config.port))
                    == Some(host)
            }
            Err(()) => false,
        };
        let valid_fetch = match fetch_site {
            Ok(None) => true,
            Ok(Some(value)) => value == "same-origin",
            Err(()) => false,
        };
        let browser_without_origin = matches!(origin, Ok(None))
            && matches!(fetch_site, Ok(None))
            && (request.headers().contains_key("sec-fetch-mode")
                || request.headers().contains_key("sec-fetch-dest")
                || request.headers().contains_key("sec-fetch-user")
                || request
                    .headers()
                    .get_all(header::USER_AGENT)
                    .iter()
                    .any(|value| {
                        value
                            .to_str()
                            .map_or(true, |value| value.contains("Mozilla/"))
                    }));
        if !valid_origin || !valid_fetch || browser_without_origin {
            return Problem::new(
                StatusCode::FORBIDDEN,
                "Cross-origin or invalid browser request refused.",
                "Use the same loopback origin as the server, or a local non-browser client.",
            )
            .into_response();
        }
    }

    let limit = body_limit(request.uri().path(), &config);
    let (parts, body) = request.into_parts();
    let body = tokio::select! {
        biased;
        () = config.app_state.wait_for_shutdown() => {
            return Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "The server is shutting down before the request body was received.",
                "Retry the request after the server restarts.",
            ).into_response();
        }
        result = tokio::time::timeout(BODY_READ_TIMEOUT, to_bytes(body, limit)) => match result {
            Ok(result) => result,
            Err(_) => return Problem::new(
                StatusCode::REQUEST_TIMEOUT,
                "The request body was not received within 30 seconds.",
                "Send the complete request body promptly and try again.",
            ).into_response(),
        },
    };
    match body {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(error) => {
            let too_large = error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            let (status, detail, recovery) = if too_large {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "The request body exceeds this route's size limit.",
                    "Send a smaller request or adjust the upload limit for uploads.",
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    "The request body could not be read.",
                    "Check the request and try again.",
                )
            };
            Problem::new(status, detail, recovery).into_response()
        }
    }
}

pub(crate) async fn response_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; form-action 'self'"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, sync::Arc, task::Poll};

    use axum::{body::Bytes, http::Request};
    use futures_util::stream;
    use genaimage_comfyui::ComfyUiClient;
    use genaimage_core::{Harness, HarnessConfig};
    use tempfile::TempDir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Notify,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::build_app;

    async fn state(workspace: &TempDir) -> AppState {
        let config = HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| {
            (key == "HOME").then(|| workspace.path().to_str().unwrap().to_owned())
        })
        .unwrap();
        let backend = ComfyUiClient::new(&config.comfyui).unwrap();
        AppState::new(Arc::new(
            Harness::new_async(config, Arc::new(backend)).await.unwrap(),
        ))
    }

    fn stalled_body(polled: Arc<Notify>) -> Body {
        Body::from_stream(stream::poll_fn(move |_| {
            polled.notify_one();
            Poll::<Option<Result<Bytes, Infallible>>>::Pending
        }))
    }

    #[tokio::test]
    async fn stalled_body_times_out() {
        let workspace = TempDir::new().unwrap();
        let state = state(&workspace).await;
        let router = build_app(state, 3000);
        let polled = Arc::new(Notify::new());
        tokio::time::pause();
        let request = Request::post("/api/v1/health")
            .header(header::HOST, "127.0.0.1:3000")
            .body(stalled_body(Arc::clone(&polled)))
            .unwrap();
        let response = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        polled.notified().await;
        tokio::time::advance(BODY_READ_TIMEOUT).await;
        let response = tokio::time::timeout(Duration::from_secs(1), response)
            .await
            .expect("body read must time out")
            .unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }

    #[tokio::test]
    async fn stalled_body_is_cancelled_during_shutdown() {
        let workspace = TempDir::new().unwrap();
        let state = state(&workspace).await;
        let router = build_app(state.clone(), 3000);
        let draining_router = router.clone();
        let polled = Arc::new(Notify::new());
        let request = Request::post("/api/v1/health")
            .header(header::HOST, "127.0.0.1:3000")
            .body(stalled_body(Arc::clone(&polled)))
            .unwrap();
        let response = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        polled.notified().await;
        state.shutdown.send_replace(true);
        let response = tokio::time::timeout(Duration::from_secs(2), response)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let request = Request::post("/api/v1/health")
            .header(header::HOST, "127.0.0.1:3000")
            .body(Body::empty())
            .unwrap();
        let response = draining_router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_stalled_http_client_does_not_block_graceful_shutdown() {
        let workspace = TempDir::new().unwrap();
        let state = state(&workspace).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = build_app(state.clone(), addr.port());
        let shutdown = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.wait_for_shutdown().await })
                .await
                .unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                format!(
                    "POST /api/v1/health HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 100\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            client.read_to_end(&mut response)
        )
        .await
        .is_err());
        state.shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            response.starts_with(b"HTTP/1.1 503"),
            "{}",
            String::from_utf8_lossy(&response)
        );
    }
}
