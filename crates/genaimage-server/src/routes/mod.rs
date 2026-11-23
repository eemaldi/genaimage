mod events;
mod health;
mod runs;
mod settings;
mod workflows;

use axum::{http::StatusCode, Router};

use crate::{AppState, Problem};

pub(crate) fn api() -> Router<AppState> {
    Router::new()
        .merge(health::routes())
        .merge(events::routes())
        .merge(runs::routes())
        .merge(settings::routes())
        .merge(workflows::routes())
        .fallback(|| async {
            Problem::new(
                StatusCode::NOT_FOUND,
                "No API endpoint matches this path.",
                "Check the path and API version.",
            )
        })
        .method_not_allowed_fallback(|| async {
            Problem::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "This endpoint does not support the request method.",
                "Use a method listed in the Allow response header.",
            )
        })
}
