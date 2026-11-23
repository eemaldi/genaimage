use axum::{
    extract::{rejection::PathRejection, Path, Request, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use genaimage_api::UploadResponse;
use genaimage_core::uploads::{UploadError, UploadMetadata};

use crate::{AppState, Problem};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/uploads", post(upload))
        .route("/uploads/{id}", get(metadata))
}

const SEND_FILE: &str = "Send multipart/form-data with exactly one file field named 'file'.";

async fn upload(
    State(state): State<AppState>,
    request: Request,
) -> Result<impl IntoResponse, Problem> {
    let boundary = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| multer::parse_boundary(value).ok())
        .ok_or_else(|| bad_request("Invalid multipart request."))?;
    let mut body = request.into_body().into_data_stream();
    // Give the parser a chance to process each chunk even if the body is
    // immediately ready; multer otherwise reads ahead while the stream is ready.
    let stream = futures_util::stream::unfold(&mut body, |body| async {
        tokio::task::yield_now().await;
        body.next().await.map(|chunk| (chunk, body))
    });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut field = multipart
        .next_field()
        .await
        .map_err(|error| multipart_problem(&error))?
        .filter(|field| {
            field.name() == Some("file") && field.file_name().is_some_and(|name| !name.is_empty())
        })
        .ok_or_else(|| bad_request("A file field with a filename is required."))?;
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

async fn metadata(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<UploadResponse>, Problem> {
    let Path(id) = path.map_err(|_| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid upload ID path.",
            "Use the Location returned by the uploads API.",
        )
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
