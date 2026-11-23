use std::error::Error as _;

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::Problem;

const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct SecurityConfig {
    pub port: u16,
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

fn allowed_host(host: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|name| host.eq_ignore_ascii_case(&format!("{name}:{port}")))
}

pub(crate) async fn protect(
    State(config): State<SecurityConfig>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let host = match single_header(request.headers(), "host") {
        Ok(Some(host)) if allowed_host(host, config.port) => host,
        _ => {
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
        .is_some_and(|authority| !authority.as_str().eq_ignore_ascii_case(host))
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
            Ok(Some(value)) => value.eq_ignore_ascii_case(&format!("http://{host}")),
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
    match to_bytes(body, limit).await {
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
