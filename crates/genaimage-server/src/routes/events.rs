use std::time::Duration;

use axum::{
    extract::{rejection::QueryRejection, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Router,
};
use futures_util::{stream, StreamExt};
use genaimage_core::{Event as CoreEvent, ReceiveError, ResyncReason};
use serde::Deserialize;
use serde_json::json;

use crate::{AppState, Problem};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/events", get(events))
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct EventQuery {
    run_id: Option<String>,
    last_event_id: Option<String>,
}

fn invalid_cursor() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "The last event ID is invalid.",
        "Supply one unsigned decimal event ID in Last-Event-ID or last_event_id.",
    )
}

#[utoipa::path(
    get, path = "/api/v1/events",
    operation_id = "events_events",
    description = "SSE event names: run_queued, run_started, run_progress, run_output, run_completed, run_failed, run_cancelled, run_unknown_submission, backend_health_changed, settings_changed; resync terminates the stream. Ordinary data uses the EventEnvelope schema; resync data uses the ResyncEvent schema (see components/schemas). Connected and heartbeat frames are comments.",
    params(EventQuery, ("Last-Event-ID" = Option<String>, Header, description = "Unsigned decimal replay cursor")),
    responses(
        (status = 200, description = "SSE frames; JSON data is EventEnvelope or ResyncEvent", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<EventQuery>, QueryRejection>,
) -> Result<Response, Problem> {
    let Query(query) = query.map_err(|_| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "The event stream query is invalid.",
            "Supply at most one run_id and one last_event_id query parameter.",
        )
    })?;
    let mut ids = headers.get_all("last-event-id").iter();
    let cursor = match ids.next() {
        Some(value) => {
            if ids.next().is_some() {
                return Err(invalid_cursor());
            }
            Some(value.to_str().map_err(|_| invalid_cursor())?)
        }
        None => query.last_event_id.as_deref(),
    }
    .map(|value| {
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_cursor());
        }
        value.parse::<u64>().map_err(|_| invalid_cursor())
    })
    .transpose()?;

    Ok(event_response(
        state,
        query.run_id,
        cursor,
        HEARTBEAT_INTERVAL,
    ))
}

fn event_response(
    state: AppState,
    run_id: Option<String>,
    cursor: Option<u64>,
    heartbeat_interval: Duration,
) -> Response {
    let harness = state.harness();
    let bus = harness.events();
    // The core takes the replay snapshot and subscribes to live delivery under
    // the same lock, so the two cannot overlap or leave a gap.
    let subscription = match cursor {
        Some(id) => bus.subscribe_since(id),
        None => Ok(bus.subscribe()),
    };
    let events = stream::unfold(Some((run_id, subscription)), |next| async move {
        let (run_id, subscription) = next?;
        let mut subscription = match subscription {
            Ok(subscription) => subscription,
            Err(reason) => return Some((resync_event(reason), None)),
        };
        loop {
            match subscription.recv().await {
                Ok(envelope) => {
                    let (kind, event_run_id) = kind_and_run(&envelope.event);
                    if run_id.is_none() || run_id.as_deref() == event_run_id {
                        let event = Event::default()
                            .id(envelope.id.to_string())
                            .event(kind)
                            .json_data(envelope);
                        return Some((event, Some((run_id, Ok(subscription)))));
                    }
                }
                Err(ReceiveError::ResyncRequired(reason)) => {
                    return Some((resync_event(reason), None));
                }
                Err(ReceiveError::Closed) => return None,
            }
        }
    });
    // Flush the HTTP response immediately even if the live subscription is idle.
    let stream = stream::once(async { Ok(Event::default().comment("connected")) })
        .chain(events)
        .take_until(async move { state.wait_for_shutdown().await });
    (
        [
            (header::CACHE_CONTROL.as_str(), "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(heartbeat_interval)
                .text("heartbeat"),
        ),
    )
        .into_response()
}

fn kind_and_run(event: &CoreEvent) -> (&'static str, Option<&str>) {
    match event {
        CoreEvent::RunQueued { run } => ("run_queued", Some(&run.id)),
        CoreEvent::RunStarted { run } => ("run_started", Some(&run.id)),
        CoreEvent::RunProgress { run, .. } => ("run_progress", Some(&run.id)),
        CoreEvent::RunOutput { run_id, .. } => ("run_output", Some(run_id)),
        CoreEvent::RunCompleted { run } => ("run_completed", Some(&run.id)),
        CoreEvent::RunFailed { run } => ("run_failed", Some(&run.id)),
        CoreEvent::RunCancelled { run } => ("run_cancelled", Some(&run.id)),
        CoreEvent::RunUnknownSubmission { run } => ("run_unknown_submission", Some(&run.id)),
        CoreEvent::BackendHealthChanged { .. } => ("backend_health_changed", None),
        CoreEvent::SettingsChanged { .. } => ("settings_changed", None),
    }
}

fn resync_event(reason: ResyncReason) -> Result<Event, axum::Error> {
    let data = match reason {
        ResyncReason::CursorOutOfRange {
            requested,
            oldest,
            latest,
        } => json!({
            "reason": "cursor_out_of_range", "requested": requested, "oldest": oldest, "latest": latest,
        }),
        ResyncReason::Lagged { skipped } => json!({ "reason": "lagged", "skipped": skipped }),
        ResyncReason::BusChanged { requested, current } => {
            json!({ "reason": "bus_changed", "requested": requested, "current": current })
        }
        ResyncReason::Invalidated { id } => json!({ "reason": "invalidated", "id": id }),
    };
    // An empty ID clears EventSource's reconnect cursor; it is not a bus event.
    Event::default().event("resync").id("").json_data(data)
}
