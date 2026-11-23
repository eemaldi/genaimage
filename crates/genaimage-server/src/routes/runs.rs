use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use genaimage_api as api;
use genaimage_core::{HarnessError, RunRecord, RunRequest, RunStatus};

use crate::{AppState, Problem};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/runs", get(list).post(submit))
        .route("/runs/reconcile", post(reconcile_all))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/events", get(events))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/reconcile", post(reconcile))
}

async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<api::SubmitRunRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let mut keys = headers.get_all("idempotency-key").iter();
    let key = keys
        .next()
        .and_then(|value| value.to_str().ok())
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 128
                && key.bytes().all(|b| b.is_ascii_graphic() && b != b',')
        });
    let Some(key) = key.filter(|_| keys.next().is_none()) else {
        return Err(Problem::new(StatusCode::BAD_REQUEST,
            "A single valid Idempotency-Key header is required.",
            "Send a unique key of 1–128 visible ASCII characters (excluding commas), and keep it for retries."));
    };
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with a workflow ID and any explicit parameter bindings.",
        )
    })?;
    let key = key.to_owned();
    let harness = state.harness();
    let task_state = state.clone();
    let result = run_mutation(&state, async move {
        let result = harness.submit_keyed_until(key, RunRequest {
            workflow: body.workflow,
            preset: body.preset,
            parameters: body.parameters,
            version: body.version,
        }, &task_state.lifecycle.deadline).await?;
        if !result.replayed && matches!(result.run.status, RunStatus::Queued | RunStatus::Cancelling) {
            let id = result.run.id.clone();
            let monitor_state = task_state.clone();
            task_state.spawn_run_task(async move {
                let lifecycle = &monitor_state.lifecycle;
                if let Err(error) = harness.monitor_run(&id, &lifecycle.shutdown, &lifecycle.deadline).await {
                    tracing::warn!(run_id = id, %error, "Run monitoring stopped; reconcile to refresh the state");
                }
            });
        }
        Ok::<_, HarnessError>(result)
    }).await?;
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let location = format!("/api/v1/runs/{}", result.run.id);
    Ok((
        status,
        [(header::LOCATION, location)],
        Json(run_response(result.run)),
    ))
}

async fn list(
    State(state): State<AppState>,
    query: Result<Query<api::RunListQuery>, QueryRejection>,
) -> Result<Json<api::RunListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .repository()
        .as_async()
        .list_page(genaimage_core::persistence::RunListQuery {
            status: query.status.map(core_status),
            workflow: query.workflow,
            created_from: query.created_from,
            created_to: query.created_to,
            limit: query.limit,
            cursor: query.cursor,
        })
        .await
        .map_err(problem)?;
    Ok(Json(api::RunListResponse {
        runs: page.runs.into_iter().map(run_response).collect(),
        next_cursor: page.next_cursor,
    }))
}

async fn get_run(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::RunResponse>, Problem> {
    state
        .harness()
        .repository()
        .as_async()
        .get(&parse_id(id)?)
        .await
        .map(|run| Json(run_response(run)))
        .map_err(problem)
}

async fn events(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::RunEventsQuery>, QueryRejection>,
) -> Result<Json<api::RunEventsResponse>, Problem> {
    let events = state
        .harness()
        .repository()
        .as_async()
        .recent_events(parse_id(id)?, parse_query(query)?.limit)
        .await
        .map_err(problem)?;
    Ok(Json(api::RunEventsResponse {
        events: events
            .into_iter()
            .map(|event| api::RunEventResponse {
                id: event.id,
                run_id: event.run_id,
                event_type: event.event_type,
                message: event.message,
                payload: event.payload,
                created_at: event.created_at,
            })
            .collect(),
    }))
}

async fn cancel(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::RunResponse>, Problem> {
    let id = parse_id(id)?;
    let harness = state.harness();
    let deadline = state.lifecycle.deadline.clone();
    let run = run_mutation(
        &state,
        async move { harness.cancel_until(&id, &deadline).await },
    )
    .await?;
    Ok(Json(run_response(run)))
}

async fn reconcile(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::RunResponse>, Problem> {
    let id = parse_id(id)?;
    let harness = state.harness();
    let deadline = state.lifecycle.deadline.clone();
    let result = run_mutation(&state, async move {
        harness.reconcile_until(&id, &deadline).await
    })
    .await?;
    Ok(Json(run_response(result.run)))
}

async fn reconcile_all(
    State(state): State<AppState>,
) -> Result<Json<api::ReconcileRunsResponse>, Problem> {
    let harness = state.harness();
    let deadline = state.lifecycle.deadline.clone();
    let results = run_mutation(&state, async move {
        harness.reconcile_all_until(&deadline).await
    })
    .await?;
    Ok(Json(api::ReconcileRunsResponse {
        runs: results
            .into_iter()
            .map(|result| run_response(result.run))
            .collect(),
    }))
}

async fn run_mutation<T: Send + 'static>(
    state: &AppState,
    operation: impl std::future::Future<Output = genaimage_core::Result<T>> + Send + 'static,
) -> Result<T, Problem> {
    state
        .spawn_run_task(operation)
        .ok_or_else(|| problem(HarnessError::ShuttingDown))?
        .await
        .map_err(|error| {
            problem(HarnessError::Database(format!(
                "run mutation task failed: {error}"
            )))
        })?
        .map_err(problem)
}

fn parse_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, Problem> {
    query.map(|Query(query)| query).map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Check the query parameters and try again.",
        )
    })
}

fn parse_id(id: Result<Path<String>, PathRejection>) -> Result<String, Problem> {
    id.map(|Path(id)| id).map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Use a run ID returned by the runs API.",
        )
    })
}

fn problem(error: HarnessError) -> Problem {
    let (status, recovery) = match &error {
        HarnessError::ShuttingDown => (
            StatusCode::SERVICE_UNAVAILABLE,
            "After restart, use the same key for submissions and reconcile before retrying cancellation.",
        ),
        HarnessError::UnsupportedOperation(_) => (
            StatusCode::BAD_GATEWAY,
            "Upgrade the configured ComfyUI or manage the run in ComfyUI itself, then reconcile.",
        ),
        HarnessError::RunNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "Use a run ID returned by the runs API.",
        ),
        HarnessError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "Use the original request with this key, or a fresh key for a distinct run.",
        ),
        HarnessError::RunConflict(_) => (
            StatusCode::CONFLICT,
            "Reconcile the run and review its status before requesting cancellation again.",
        ),
        HarnessError::InvalidRunRequest(_) => (
            StatusCode::BAD_REQUEST,
            "Check the runs API request fields and try again.",
        ),
        HarnessError::Workflow(_) | HarnessError::InvalidPath { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Check the configured workflow ID, preset and parameter bindings.",
        ),
        HarnessError::CancellationUnknown(_) => (
            StatusCode::BAD_GATEWAY,
            "Reconcile the run before taking further action; do not blindly retry cancellation.",
        ),
        HarnessError::CancellationNotSent(_) | HarnessError::CancellationRejected(_) => (
            StatusCode::BAD_GATEWAY,
            "Check ComfyUI and reconcile the run before requesting cancellation again.",
        ),
        HarnessError::Backend { .. } | HarnessError::ComfyUi(_) => (
            StatusCode::BAD_GATEWAY,
            "Check ComfyUI health and reconcile any affected run; do not blindly resubmit.",
        ),
        _ => {
            tracing::error!(%error, "Runs API request failed");
            return Problem::new(StatusCode::INTERNAL_SERVER_ERROR, "The server could not complete the run operation.",
                "Check the server logs and local state. For submission, keep the same idempotency key rather than trying a new one.");
        }
    };
    Problem::new(status, error.to_string(), recovery)
}

fn run_response(run: RunRecord) -> api::RunResponse {
    api::RunResponse {
        id: run.id,
        prompt_id: run.prompt_id,
        workflow_id: run.workflow_id,
        workflow_version: run.workflow_version,
        workflow_sha256: run.workflow_sha256,
        status: api_status(run.status),
        parameters: run.parameters,
        error: run.error,
        node_warnings: run.node_warnings,
        created_at: run.created_at,
        updated_at: run.updated_at,
        completed_at: run.completed_at,
    }
}

macro_rules! status_conversions {
    ($($status:ident),+ $(,)?) => {
        fn api_status(status: RunStatus) -> api::RunStatus {
            match status { $(RunStatus::$status => api::RunStatus::$status),+ }
        }

        fn core_status(status: api::RunStatus) -> RunStatus {
            match status { $(api::RunStatus::$status => RunStatus::$status),+ }
        }
    };
}

status_conversions!(
    Created,
    Submitting,
    UnknownSubmission,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelling,
    Cancelled
);
