#[cfg(feature = "portal")]
mod embedded;
mod proxy;

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};

use crate::{AppState, Problem};
pub use proxy::PortalDevProxy;

#[derive(Clone)]
pub(crate) struct DevPortalResponse;

pub(super) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn api_not_found() -> Response {
    Problem::new(
        StatusCode::NOT_FOUND,
        "No API endpoint matches this path.",
        "Check the path and API version.",
    )
    .into_response()
}

pub(crate) async fn fallback(State(state): State<AppState>, request: Request) -> Response {
    let head = request.method() == Method::HEAD;
    let mut response = if is_api_path(request.uri().path()) {
        api_not_found()
    } else if !matches!(*request.method(), Method::GET | Method::HEAD) {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
        )
            .into_response()
    } else if let Some(proxy) = &state.portal_dev {
        proxy.forward(request).await
    } else {
        #[cfg(feature = "portal")]
        {
            embedded::serve(request.uri().path(), request.headers())
        }
        #[cfg(not(feature = "portal"))]
        StatusCode::NOT_FOUND.into_response()
    };
    if head {
        *response.body_mut() = axum::body::Body::empty();
    }
    response
}
