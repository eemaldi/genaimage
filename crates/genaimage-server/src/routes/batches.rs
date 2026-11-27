use std::sync::{Arc, Mutex};

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
    batch::{
        BatchCounts, BatchItemView, BatchRequest, BatchSummary, BatchSweep, ItemSubmission,
        SeedSweep, BATCH_ITEM_CAP,
    },
    orchestrator::{BatchCreation, RunObserver},
    Harness,
};

use super::{
    runs::{self, idempotency_key, parse_id, parse_query, run_mutation},
    workflows::inherited_input,
};
use crate::{AppState, Problem};

#[utoipa::path(
    post, path = "/batches/preview",
    operation_id = "batches_preview",
    tag = "batches",
    description = "Expand a batch and validate every item against the pinned workflow revision without submitting anything or contacting ComfyUI. A grid is the cartesian product with parameter names sorted and the last name varying fastest. The server limit on items is `max_items_cap`. An invalid item rejects the whole batch (422 `urn:genaimage:problem:batch-items-invalid`, listing at most 20 items by index). Graph inputs that binding leaves untouched are listed once as `inherited_inputs`, because every item shares them. A preview whose values would exceed 16 MiB is rejected as `batch-too-large`.",
    request_body = api::SubmitBatchRequest,
    responses(
        (status = 200, body = api::BatchPreviewResponse),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Workflow revision mismatch or workflow ID declared by several manifests", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Invalid sweep, batch too large or invalid items", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn preview(
    State(state): State<AppState>,
    body: Result<Json<api::SubmitBatchRequest>, JsonRejection>,
) -> Result<Json<api::BatchPreviewResponse>, Problem> {
    let request = core_request(body)?;
    let plan = state
        .harness()
        .preview_batch(&request)
        .await
        .map_err(runs::problem)?;
    Ok(Json(api::BatchPreviewResponse {
        item_count: plan.items.len(),
        max_items: plan.max_items,
        max_items_cap: BATCH_ITEM_CAP,
        workflow_revision: plan.workflow_revision,
        inherited_inputs: plan
            .inherited_inputs
            .into_iter()
            .map(|input| inherited_input(input.parameter, input.node, input.input, &input.value))
            .collect(),
        items: plan
            .items
            .into_iter()
            .map(|item| api::BatchPreviewItem {
                index: item.index,
                parameters: item.parameters,
                sweep_values: item.sweep_values,
                effective_parameters: item.effective_parameters,
            })
            .collect(),
    }))
}

#[utoipa::path(
    post, path = "/batches",
    operation_id = "batches_submit",
    tag = "batches",
    description = "Validate every item against the pinned revision, then record the batch and its items atomically and return 202 with the batch. Items are submitted afterwards, one at a time, each as an ordinary run keyed `{batch id}:{index}`. A failed item does not stop later ones. An ambiguous item becomes `outcome_unknown` and is reconciled, never resubmitted. Repeating the same key and body returns 200 with the batch and resumes items that have no run; a different body with the same key is a 409. The key space is separate from the runs API.",
    params(("Idempotency-Key" = String, Header, description = "Required unique key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::SubmitBatchRequest,
    responses(
        (status = 202, description = "Accepted", body = api::BatchResponse, headers(("Location" = String, description = "Batch URL"))),
        (status = 200, description = "Idempotent replay (current state)", body = api::BatchResponse, headers(("Location" = String, description = "Batch URL"))),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Workflow revision mismatch, workflow ID declared by several manifests or idempotency key conflict", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Invalid sweep, batch too large or invalid items", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 502, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<api::SubmitBatchRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let key = idempotency_key(&headers)?;
    let request = core_request(body)?;
    let harness = state.harness();
    let started = {
        let (task_state, harness) = (state.clone(), harness.clone());
        run_mutation(&state, async move {
            let creation = harness.create_batch(key, request).await?;
            Ok(start_driver(&task_state, harness, creation).await)
        })
        .await?
    };
    let status = if started.replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok(respond(status, started))
}

#[utoipa::path(
    post, path = "/batches/{id}/resume",
    operation_id = "batches_resume",
    tag = "batches",
    description = "Submit the items that have no run: pending, `not_submitted` (for example after a restart) and `failed` ones without a `run_id`, which were rejected before any prompt was sent. Items that already have a run are never submitted again: that includes `outcome_unknown` ones and `failed` ones with a `run_id`, which failed during preparation and are final. A running driver keeps the settings it started with. Returns 202 when a driver started, or 200 with the current state when nothing needed submitting or a driver is already running. Re-posting the original key and body does the same.",
    params(("id" = String, Path, description = "Batch ID")),
    responses(
        (status = 202, description = "Resumed", body = api::BatchResponse, headers(("Location" = String, description = "Batch URL"))),
        (status = 200, description = "Nothing to resume or a driver is already running", body = api::BatchResponse, headers(("Location" = String, description = "Batch URL"))),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn resume(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<impl IntoResponse, Problem> {
    let id = batch_id(id)?;
    let harness = state.harness();
    let started = {
        let (task_state, harness) = (state.clone(), harness.clone());
        run_mutation(&state, async move {
            let creation = harness.resume_batch(&id).await?;
            Ok(start_driver(&task_state, harness, creation).await)
        })
        .await?
    };
    let status = if started.driver_started {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(respond(status, started))
}

#[utoipa::path(
    get, path = "/batches",
    operation_id = "batches_list",
    tag = "batches",
    description = "List batches, newest first, with their state and item counts.",
    params(api::BatchListQuery),
    responses(
        (status = 200, body = api::BatchListResponse),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list(
    State(state): State<AppState>,
    query: Result<Query<api::BatchListQuery>, QueryRejection>,
) -> Result<Json<api::BatchListResponse>, Problem> {
    let query = parse_query(query)?;
    let (entries, next_cursor) = state
        .harness()
        .list_batches(query.workflow, query.cursor, query.limit)
        .await
        .map_err(runs::problem)?;
    Ok(Json(api::BatchListResponse {
        batches: entries
            .into_iter()
            .map(|entry| api::BatchSummaryResponse {
                id: entry.record.id,
                created_at: entry.record.created_at,
                workflow: entry.record.workflow_id,
                workflow_revision: entry.record.workflow_revision,
                item_count: entry.record.item_count,
                state: api_state(entry.state),
                counts: api_counts(entry.counts),
            })
            .collect(),
        next_cursor,
    }))
}

#[utoipa::path(
    get, path = "/batches/{id}",
    operation_id = "batches_get_batch",
    tag = "batches",
    description = "Read a batch. Each item shows its submission state and, once it has a run, that run's status. A run is shown as completed only once ComfyUI confirmed its outputs.",
    params(("id" = String, Path, description = "Batch ID")),
    responses(
        (status = 200, body = api::BatchResponse),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get_batch(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::BatchResponse>, Problem> {
    let batch = state
        .harness()
        .get_batch(&batch_id(id)?)
        .await
        .map_err(runs::problem)?;
    Ok(Json(batch_response(batch)))
}

fn batch_id(id: Result<Path<String>, PathRejection>) -> Result<String, Problem> {
    parse_id(id).map_err(|_| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "The batch ID is invalid.",
            "Use a batch ID returned by the batches API.",
        )
    })
}

fn core_request(
    body: Result<Json<api::SubmitBatchRequest>, JsonRejection>,
) -> Result<BatchRequest, Problem> {
    let Json(body) = body.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json with workflow, workflow_revision, parameters, a sweep (seeds or grid) and max_items.",
        )
    })?;
    if let api::BatchSweep::Grid(grid) = &body.sweep {
        if let Some(name) = grid.repeated.first() {
            return Err(Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Sweep parameter '{name}' is listed more than once in the grid."),
                "List each parameter once in the grid, with all its values in one list, then preview the batch again.",
            )
            .with_type("urn:genaimage:problem:batch-invalid"));
        }
    }
    Ok(BatchRequest {
        workflow: body.workflow,
        workflow_revision: body.workflow_revision,
        preset: body.preset,
        parameters: body.parameters,
        sweep: match body.sweep {
            api::BatchSweep::Seeds(sweep) => BatchSweep::Seeds(SeedSweep {
                parameter: sweep.parameter,
                values: sweep.values,
            }),
            api::BatchSweep::Grid(grid) => BatchSweep::Grid(grid.axes),
        },
        max_items: body.max_items,
    })
}

/// A batch as recorded, with whether its key was replayed and whether a driver
/// was started for it.
struct StartedBatch {
    batch: genaimage_core::batch::BatchView,
    replayed: bool,
    driver_started: bool,
}

/// Hands the driver to a tracked task. Callers run this inside the tracked
/// mutation task, so a client that disconnects cannot drop a driver, and with
/// it the batch's claim, between creating the batch and starting it.
async fn start_driver(
    state: &AppState,
    harness: Arc<Harness>,
    creation: BatchCreation,
) -> StartedBatch {
    let BatchCreation {
        batch,
        replayed,
        driver,
    } = creation;
    let Some(driver) = driver else {
        return StartedBatch {
            batch,
            replayed,
            driver_started: false,
        };
    };
    let observer: RunObserver = {
        let (state, harness) = (state.clone(), harness.clone());
        Arc::new(move |run| state.ensure_run_monitor(harness.clone(), run.id.clone()))
    };
    let (stop, deadline) = (
        state.lifecycle.shutdown.clone(),
        state.lifecycle.deadline.clone(),
    );
    // If the server is already shutting down the task is refused, and the
    // driver stays in the slot so its items can be recorded as unsent.
    let slot = Arc::new(Mutex::new(Some(driver)));
    let task_slot = slot.clone();
    let spawned = state.spawn_run_task(async move {
        let driver = task_slot.lock().expect("driver slot").take();
        if let Some(driver) = driver {
            driver.run(stop, deadline, observer).await;
        }
    });
    if spawned.is_some() {
        return StartedBatch {
            batch,
            replayed,
            driver_started: true,
        };
    }
    let driver = slot.lock().expect("driver slot").take();
    let batch = match driver {
        Some(driver) => {
            let id = batch.record.id.clone();
            let settled = driver
                .abandon("The server was shutting down before this item was submitted.")
                .await;
            match settled {
                Ok(()) => harness.get_batch(&id).await.unwrap_or(batch),
                Err(_) => batch,
            }
        }
        None => batch,
    };
    StartedBatch {
        batch,
        replayed,
        driver_started: false,
    }
}

fn respond(status: StatusCode, started: StartedBatch) -> impl IntoResponse {
    let location = format!("/api/v1/batches/{}", started.batch.record.id);
    (
        status,
        [(header::LOCATION, location)],
        Json(batch_response(started.batch)),
    )
}

fn batch_response(batch: genaimage_core::batch::BatchView) -> api::BatchResponse {
    api::BatchResponse {
        id: batch.record.id,
        created_at: batch.record.created_at,
        workflow: batch.record.workflow_id,
        workflow_revision: batch.record.workflow_revision,
        item_count: batch.record.item_count,
        state: api_state(batch.state),
        counts: api_counts(batch.counts),
        preset: batch.request.preset,
        parameters: batch.request.parameters,
        sweep: match batch.request.sweep {
            BatchSweep::Seeds(sweep) => api::BatchSweep::Seeds(api::SeedSweep {
                parameter: sweep.parameter,
                values: sweep.values,
            }),
            BatchSweep::Grid(grid) => api::BatchSweep::Grid(api::BatchGrid {
                axes: grid,
                repeated: Vec::new(),
            }),
        },
        max_items: batch.request.max_items,
        items: batch.items.into_iter().map(item_response).collect(),
    }
}

fn item_response(item: BatchItemView) -> api::BatchItemResponse {
    api::BatchItemResponse {
        index: item.index as usize,
        parameters: item.parameters,
        sweep_values: item.sweep_values,
        run_id: item.run_id,
        submission: match item.submission {
            ItemSubmission::Pending => api::BatchItemSubmission::Pending,
            ItemSubmission::Submitted => api::BatchItemSubmission::Submitted,
            ItemSubmission::Failed => api::BatchItemSubmission::Failed,
            ItemSubmission::NotSubmitted => api::BatchItemSubmission::NotSubmitted,
            ItemSubmission::OutcomeUnknown => api::BatchItemSubmission::OutcomeUnknown,
        },
        error: item.error,
        run_status: item.run_status.map(runs::api_status),
    }
}

fn api_state(state: BatchSummary) -> api::BatchState {
    match state {
        BatchSummary::Submitting => api::BatchState::Submitting,
        BatchSummary::Interrupted => api::BatchState::Interrupted,
        BatchSummary::Submitted => api::BatchState::Submitted,
        BatchSummary::Partial => api::BatchState::Partial,
        BatchSummary::Failed => api::BatchState::Failed,
    }
}

fn api_counts(counts: BatchCounts) -> api::BatchCounts {
    api::BatchCounts {
        pending: counts.pending,
        submitted: counts.submitted,
        failed: counts.failed,
        not_submitted: counts.not_submitted,
        outcome_unknown: counts.outcome_unknown,
    }
}
