use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use axum::{
    body::Body,
    extract::Request,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};
use url::{Host, Url};

use super::{api_not_found, is_api_path, DevPortalResponse};
use crate::Problem;

/// An explicitly configured Vite development origin, restricted to loopback.
#[derive(Clone, Debug)]
pub struct PortalDevProxy {
    origin: Url,
    client: reqwest::Client,
}

impl PortalDevProxy {
    pub fn new(value: &str) -> io::Result<Self> {
        let invalid_url = || {
            io::Error::new(io::ErrorKind::InvalidInput,
            "--portal-dev-url must be a loopback HTTP origin, such as http://127.0.0.1:5173 (no credentials, path, query or fragment).")
        };
        let origin = Url::parse(value).map_err(|_| invalid_url())?;
        if origin.scheme() != "http"
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.port() == Some(0)
        {
            return Err(invalid_url());
        }
        match origin.host() {
            Some(Host::Ipv4(ip)) if ip.is_loopback() => (),
            Some(Host::Ipv6(ip)) if std::net::IpAddr::V6(ip).to_canonical().is_loopback() => (),
            Some(Host::Domain("localhost")) => (),
            _ => return Err(invalid_url()),
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                    SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
                ],
            )
            .build()
            .map_err(io::Error::other)?;
        Ok(Self { origin, client })
    }

    pub(crate) fn port(&self) -> u16 {
        self.origin.port().unwrap_or(80)
    }

    pub(super) async fn forward(&self, request: Request) -> Response {
        if request.headers().contains_key(header::UPGRADE) {
            return StatusCode::NOT_IMPLEMENTED.into_response();
        }
        let mut url = self.origin.clone();
        url.set_path(request.uri().path());
        url.set_query(request.uri().query());
        if is_api_path(url.path()) {
            return api_not_found();
        }
        let mut upstream = self.client.request(request.method().clone(), url);
        for key in [
            header::ACCEPT,
            header::ACCEPT_ENCODING,
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
        ] {
            for value in request.headers().get_all(&key) {
                upstream = upstream.header(&key, value);
            }
        }
        let upstream = match upstream.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(%error, "Vite portal request failed");
                return Problem::new(
                    StatusCode::BAD_GATEWAY,
                    "The Vite development server did not respond.",
                    "Start Vite with `npm run dev` in apps/portal and check --portal-dev-url.",
                )
                .into_response();
            }
        };
        let mut response = Response::builder().status(upstream.status());
        for key in [
            header::CONTENT_TYPE,
            header::CONTENT_ENCODING,
            header::CONTENT_LENGTH,
            header::VARY,
            header::CACHE_CONTROL,
            header::ETAG,
            header::LAST_MODIFIED,
            header::LOCATION,
        ] {
            for value in upstream.headers().get_all(&key) {
                response = response.header(&key, value);
            }
        }
        let body = if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from_stream(upstream.bytes_stream())
        };
        response
            .extension(DevPortalResponse)
            .body(body)
            .expect("valid Vite response")
    }
}
