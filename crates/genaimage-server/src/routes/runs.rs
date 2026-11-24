use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use genaimage_api as api;
use genaimage_core::{
    orchestrator::{ReconcileOutcome, ReconcileResult},
    Harness, HarnessError, KeyedSubmission, RunRecord, RunRequest, RunStatus,
};

use crate::{AppState, Problem};

#[utoipa::path(
    post, path = "/runs",
    operation_id = "runs_submit",
    tag = "runs",
    description = "Submit explicit bindings, optionally pinned to a reviewed workflow revision; use the same key for retries.",
    params(("Idempotency-Key" = String, Header, description = "Required unique key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::SubmitRunRequest,
    responses(
        (status = 202, description = "Accepted", body = api::RunResponse, headers(("Location" = String, description = "Run URL"))), (status = 200, description = "Idempotent replay", body = api::RunResponse, headers(("Location" = String, description = "Run URL"))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Workflow revision mismatch or idempotency key conflict", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<api::SubmitRunRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let key = idempotency_key(&headers)?;
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with a workflow ID and any explicit parameter bindings.",
        )
    })?;
    let deadline = state.lifecycle.deadline.clone();
    let harness = state.harness();
    let submission = {
        let harness = harness.clone();
        async move {
            harness
                .submit_keyed_until(
                    key,
                    RunRequest {
                        workflow: body.workflow,
                        preset: body.preset,
                        parameters: body.parameters,
                        version: body.version,
                        workflow_revision: body.workflow_revision,
                    },
                    &deadline,
                )
                .await
        }
    };
    submit_and_monitor(state, harness, submission, StatusCode::ACCEPTED).await
}

#[utoipa::path(
    post, path = "/runs/{id}/rerun",
    operation_id = "runs_rerun",
    tag = "runs",
    description = "Create a new run with the source run’s verified workflow, preset, resolved parameters and inputs. The source run is not changed. Send an empty request body.",
    params(("id" = String, Path, description = "Source run ID"), ("Idempotency-Key" = String, Header, description = "Required unique key, 1–128 visible ASCII characters excluding commas")),
    responses(
        (status = 201, description = "New run created", body = api::RunResponse, headers(("Location" = String, description = "New run URL"))),
        (status = 200, description = "Idempotent replay (current state of the new run)", body = api::RunResponse, headers(("Location" = String, description = "New run URL"))),
        (status = 400, description = "Non-empty request body, invalid key or invalid path", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Source run not found", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Fingerprint mismatch or idempotency key conflict", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn rerun(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    let key = idempotency_key(&headers)?;
    let id = parse_id(id)?;
    let deadline = state.lifecycle.deadline.clone();
    let harness = state.harness();
    let submission = {
        let harness = harness.clone();
        async move { harness.rerun_until(key, id, &deadline).await }
    };
    submit_and_monitor(state, harness, submission, StatusCode::CREATED).await
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, Problem> {
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
    Ok(key.to_owned())
}

async fn submit_and_monitor(
    state: AppState,
    harness: std::sync::Arc<Harness>,
    submission: impl std::future::Future<Output = genaimage_core::Result<KeyedSubmission>>
        + Send
        + 'static,
    created_status: StatusCode,
) -> Result<impl IntoResponse, Problem> {
    let task_state = state.clone();
    let result = run_mutation(&state, async move {
        let result = submission.await?;
        if !result.replayed
            && matches!(result.run.status, RunStatus::Queued | RunStatus::Cancelling)
        {
            task_state.ensure_run_monitor(harness, result.run.id.clone());
        }
        Ok::<_, HarnessError>(result)
    })
    .await?;
    let status = if result.replayed {
        StatusCode::OK
    } else {
        created_status
    };
    let location = format!("/api/v1/runs/{}", result.run.id);
    Ok((
        status,
        [(header::LOCATION, location)],
        Json(run_response(result.run)),
    ))
}

#[utoipa::path(
    get, path = "/runs",
    operation_id = "runs_list",
    tag = "runs",
    description = "List runs.",
    params(api::RunListQuery),
    responses(
        (status = 200, body = api::RunListResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(
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

#[utoipa::path(
    get, path = "/runs/{id}",
    operation_id = "runs_get_run",
    tag = "runs",
    description = "Read a run.",
    params(("id" = String, Path, description = "Run ID")),
    responses(
        (status = 200, body = api::RunResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get_run(
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

#[utoipa::path(
    get, path = "/runs/{id}/events",
    operation_id = "runs_events",
    tag = "runs",
    description = "Read persisted run events.",
    params(("id" = String, Path, description = "Run ID"), api::RunEventsQuery),
    responses(
        (status = 200, body = api::RunEventsResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn events(
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

#[utoipa::path(
    post, path = "/runs/{id}/cancel",
    operation_id = "runs_cancel",
    tag = "runs",
    description = "Cancel a run.",
    params(("id" = String, Path, description = "Run ID")),
    responses(
        (status = 200, body = api::RunResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn cancel(
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

#[utoipa::path(
    post, path = "/runs/{id}/reconcile",
    operation_id = "runs_reconcile",
    tag = "runs",
    description = "Reconcile a run against ComfyUI.",
    params(("id" = String, Path, description = "Run ID")),
    responses(
        (status = 200, body = api::ReconcileRunResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn reconcile(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::ReconcileRunResponse>, Problem> {
    let id = parse_id(id)?;
    let harness = state.harness();
    let task_state = state.clone();
    let result = run_mutation(&state, async move {
        harness
            .reconcile_until_with_active(&id, &task_state.lifecycle.deadline, &|run| {
                task_state.ensure_run_monitor(harness.clone(), run.id.clone());
            })
            .await
    })
    .await?;
    Ok(Json(reconcile_response(result)))
}

#[utoipa::path(
    post, path = "/runs/reconcile",
    operation_id = "runs_reconcile_all",
    tag = "runs",
    description = "Reconcile runs against ComfyUI.",
    responses(
        (status = 200, body = api::ReconcileRunsWithOutcomesResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn reconcile_all(
    State(state): State<AppState>,
) -> Result<Json<api::ReconcileRunsWithOutcomesResponse>, Problem> {
    let harness = state.harness();
    let task_state = state.clone();
    let results = run_mutation(&state, async move {
        harness
            .reconcile_all_until_with_active(&task_state.lifecycle.deadline, &|run| {
                task_state.ensure_run_monitor(harness.clone(), run.id.clone());
            })
            .await
    })
    .await?;
    Ok(Json(api::ReconcileRunsWithOutcomesResponse {
        runs: results.into_iter().map(reconcile_response).collect(),
    }))
}

fn reconcile_response(result: ReconcileResult) -> api::ReconcileRunResponse {
    api::ReconcileRunResponse {
        run: run_response(result.run),
        settles_at: result.settles_at,
        outcome: match result.outcome {
            ReconcileOutcome::Terminal => api::ReconcileOutcome::Terminal,
            ReconcileOutcome::Active => api::ReconcileOutcome::Active,
            ReconcileOutcome::InProgressElsewhere => api::ReconcileOutcome::InProgressElsewhere,
            ReconcileOutcome::NotFound => api::ReconcileOutcome::NotFound,
            ReconcileOutcome::StillAmbiguous => api::ReconcileOutcome::StillAmbiguous,
        },
    }
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
    id.map(|Path(id)| id)
        .map_err(|error| Problem::invalid_path(error, "Use a run ID returned by the runs API."))
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
        HarnessError::WorkflowChanged => return Problem::workflow_changed(),
        HarnessError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "Use the original request with this key, or a fresh key for a distinct run.",
        ),
        HarnessError::RerunUnverifiedMedia { .. } | HarnessError::RerunMissingManifest => {
            let recovery = if matches!(error, HarnessError::RerunMissingManifest) {
                "Use Remix to review the saved parameters and submit a new run."
            } else {
                "Bind the ComfyUI-side file as an image upload parameter where supported, or use Remix to review the workflow and submit a new run."
            };
            return Problem::new(StatusCode::CONFLICT, error.to_string(), recovery)
                .with_type("urn:genaimage:problem:rerun-mismatch");
        }
        HarnessError::RerunMismatch(_) => return Problem::new(
            StatusCode::CONFLICT,
            error.to_string(),
            "Restore the original workflow and input files, or use Remix to review the current workflow and select the inputs again.",
        ).with_type("urn:genaimage:problem:rerun-mismatch"),
        HarnessError::RunConflict(_) => (
            StatusCode::CONFLICT,
            "Reconcile the run and review its status before requesting cancellation again.",
        ),
        HarnessError::InvalidRunRequest(_) => (
            StatusCode::BAD_REQUEST,
            "Check the runs API request fields and try again.",
        ),
        HarnessError::WorkflowDiscoveryIncomplete { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Fix the workflow directory permissions or repair the path, then retry with the same idempotency key.",
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
    let workflow_revision = run.workflow_revision();
    api::RunResponse {
        id: run.id,
        prompt_id: run.prompt_id,
        workflow_id: run.workflow_id,
        workflow_version: run.workflow_version,
        workflow_sha256: run.workflow_sha256,
        workflow_revision,
        rerun_of: run.rerun_of,
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

        pub(super) fn core_status(status: api::RunStatus) -> RunStatus {
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
