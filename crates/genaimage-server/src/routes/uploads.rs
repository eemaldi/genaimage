use super::UploadBody;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{
    body::Bytes,
    extract::{rejection::PathRejection, Path, Request, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use futures_util::StreamExt;
use genaimage_api::UploadResponse;
use genaimage_core::uploads::{UploadError, UploadMetadata};

use crate::{AppState, Problem};

const FRAMING_LIMIT: usize = 16 * 1024;
const PARSER_CHUNK: usize = 4 * 1024;
const MAX_BOUNDARY: usize = 70;
const FILE_DATA: usize = usize::MAX;

#[derive(Debug)]
struct FramingTooLarge;

impl std::fmt::Display for FramingTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("The multipart framing exceeds 16 KiB.")
    }
}

impl std::error::Error for FramingTooLarge {}

const SEND_FILE: &str = "Send multipart/form-data with exactly one file field named 'file'.";

#[utoipa::path(
    post, path = "/uploads",
    operation_id = "uploads_upload",
    tag = "uploads",
    description = "Upload a PNG, JPEG or WebP reference image.",
    request_body(content = UploadBody, content_type = "multipart/form-data"),
    responses(
        (status = 201, body = genaimage_api::UploadResponse, headers(("Location" = String, description = "Upload metadata URL"))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn upload(
    State(state): State<AppState>,
    request: Request,
) -> Result<impl IntoResponse, Problem> {
    let boundary = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| multer::parse_boundary(value).ok())
        .filter(|boundary| !boundary.is_empty() && boundary.len() <= MAX_BOUNDARY)
        .ok_or_else(|| bad_request("Invalid multipart request."))?;
    let mut body = request.into_body().into_data_stream();
    // While next_field is looking for a part, bound the bytes offered to
    // multer, which buffers the preamble and headers until their terminators.
    let budget = AtomicUsize::new(FRAMING_LIMIT);
    let stream = futures_util::stream::unfold(
        (&mut body, Bytes::new(), &budget),
        |(body, mut pending, budget)| async move {
            // Multer otherwise reads ahead while the stream remains ready.
            tokio::task::yield_now().await;
            let remaining = budget.load(Ordering::Relaxed);
            if remaining == 0 {
                return Some((
                    Err(axum::BoxError::from(FramingTooLarge)),
                    (body, pending, budget),
                ));
            }
            while pending.is_empty() {
                match body.next().await? {
                    Ok(chunk) => pending = chunk,
                    Err(error) => return Some((Err(error.into()), (body, pending, budget))),
                }
            }
            let count = pending.len().min(PARSER_CHUNK).min(remaining);
            if remaining != FILE_DATA {
                budget.store(remaining - count, Ordering::Relaxed);
            }
            Some((Ok(pending.split_to(count)), (body, pending, budget)))
        },
    );
    let constraints = multer::Constraints::new().allowed_fields(vec!["file"]);
    let mut multipart = multer::Multipart::with_constraints(stream, boundary, constraints);
    let mut field = multipart
        .next_field()
        .await
        .map_err(|error| multipart_problem(&error))?
        .filter(|field| {
            field.name() == Some("file") && field.file_name().is_some_and(|name| !name.is_empty())
        })
        .ok_or_else(|| bad_request("A file field with a filename is required."))?;
    budget.store(FILE_DATA, Ordering::Relaxed);
    let store = state.harness().uploads();
    let mut staged = store
        .stage(field.file_name().expect("checked filename"))
        .await
        .map_err(storage_problem)?;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|error| multipart_problem(&error))?
    {
        staged.write_chunk(&chunk).await.map_err(storage_problem)?;
    }
    drop(field);
    // Multer can retain a partial delimiter (CRLF + "--" + boundary)
    // and one chunk that already contains part of the next header.
    budget.store(
        FRAMING_LIMIT - PARSER_CHUNK - MAX_BOUNDARY - 4,
        Ordering::Relaxed,
    );
    if multipart
        .next_field()
        .await
        .map_err(|error| multipart_problem(&error))?
        .is_some()
    {
        return Err(bad_request("Only one file field is accepted."));
    }
    // The parser can stop at the boundary, but the limit applies to the whole body.
    drop(multipart);
    while let Some(chunk) = body.next().await {
        chunk.map_err(|error| multipart_problem(&error))?;
    }
    let stored = staged.publish().await.map_err(storage_problem)?;
    let location = format!("/api/v1/uploads/{}", stored.upload_id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(response(stored)),
    ))
}

#[utoipa::path(
    get, path = "/uploads/{id}",
    operation_id = "uploads_metadata",
    tag = "uploads",
    description = "Read upload metadata.",
    params(("id" = String, Path, description = "Upload ID")),
    responses(
        (status = 200, body = genaimage_api::UploadResponse),
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
pub(super) async fn metadata(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<UploadResponse>, Problem> {
    let Path(id) = path.map_err(|error| {
        Problem::invalid_path(error, "Use the Location returned by the uploads API.")
    })?;
    let stored = state
        .harness()
        .uploads()
        .metadata(&id)
        .await
        .map_err(storage_problem)?
        .ok_or_else(|| {
            Problem::new(
                StatusCode::NOT_FOUND,
                "Upload not found.",
                "Use an ID returned by the uploads API, or upload the reference again.",
            )
        })?;
    Ok(Json(response(stored)))
}

fn response(stored: UploadMetadata) -> UploadResponse {
    UploadResponse {
        upload_id: stored.upload_id,
        sha256: stored.sha256,
        byte_size: stored.byte_size,
        media_type: stored.media_type,
        original_filename: stored.original_filename,
    }
}

fn bad_request(detail: &str) -> Problem {
    Problem::new(StatusCode::BAD_REQUEST, detail, SEND_FILE)
}

fn multipart_problem(mut error: &(dyn std::error::Error + 'static)) -> Problem {
    loop {
        if error.is::<FramingTooLarge>() {
            return Problem::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                error.to_string(),
                "Send the file without a large multipart preamble or part headers.",
            );
        }
        if error.is::<http_body_util::LengthLimitError>() {
            return storage_problem(UploadError::TooLarge);
        }
        // Multer exposes stream read failures as a variant rather than a source.
        if let Some(multer::Error::StreamReadFailed(inner)) = error.downcast_ref::<multer::Error>()
        {
            error = inner.as_ref();
        } else if let Some(source) = error.source() {
            error = source;
        } else {
            return bad_request("The multipart request is incomplete or could not be read.");
        }
    }
}

fn storage_problem(error: UploadError) -> Problem {
    match error {
        UploadError::UnsafeEntry => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
            "Inspect the configured uploads directory and move the unexpected entry out of the store before uploading the image again.",
        ),
        UploadError::TooLarge => Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            error.to_string(),
            "Use a smaller image or adjust [server] max_upload_bytes and restart the server.",
        ),
        UploadError::UnsupportedType => Problem::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            error.to_string(),
            "Choose a PNG, JPEG or WebP image.",
        ),
        _ => {
            tracing::error!(%error, "Upload storage operation failed");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not access the upload store.",
                "Check the server logs and the configured uploads directory.",
            )
        }
    }
}
