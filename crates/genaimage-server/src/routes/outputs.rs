use axum::{
    body::Body,
    extract::{
        rejection::{PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use genaimage_api as api;
use genaimage_core::{
    media::{MediaFile, OutputListQuery, ThumbnailSize},
    HarnessError,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{AppState, Problem};

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "private, no-cache";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/outputs", get(list))
        .route("/outputs/{id}", get(original))
        .route("/outputs/{id}/thumbnail", get(thumbnail))
}

async fn list(
    State(state): State<AppState>,
    query: Result<Query<api::OutputListQuery>, QueryRejection>,
) -> Result<Json<api::OutputListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .repository()
        .as_async()
        .list_outputs(OutputListQuery {
            run: query.run,
            workflow: query.workflow,
            created_from: query.created_from,
            created_to: query.created_to,
            tag: query.tag,
            limit: query.limit,
            cursor: query.cursor,
        })
        .await
        .map_err(problem)?;
    Ok(Json(api::OutputListResponse {
        outputs: page
            .outputs
            .into_iter()
            .map(|indexed| {
                let output = indexed.output;
                api::OutputResponse {
                    id: indexed.id,
                    workflow_id: indexed.workflow_id,
                    tags: indexed.tags,
                    content_type: content_type(&output.source_filename).into(),
                    run_id: output.run_id,
                    node_id: output.node_id,
                    kind: output.kind,
                    source_filename: output.source_filename,
                    sha256: output.sha256,
                    byte_size: output.byte_size,
                    created_at: output.created_at,
                }
            })
            .collect(),
        next_cursor: page.next_cursor,
    }))
}

async fn original(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let (output, file) = state
        .harness()
        .open_output(&parse_id(id)?)
        .await
        .map_err(problem)?;
    let ranged = method == Method::GET
        && headers.get(header::IF_RANGE).map_or(true, |value| {
            value.as_bytes() == format!("\"{}\"", file.sha256).as_bytes()
        });
    let range = ranged.then_some(&headers);
    respond(
        file,
        content_type(&output.output.source_filename),
        IMMUTABLE,
        true,
        range,
        &headers,
    )
    .await
}

async fn thumbnail(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::ThumbnailQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let size = ThumbnailSize::new(parse_query(query)?.size).map_err(problem)?;
    let file = state
        .harness()
        .open_thumbnail(&parse_id(id)?, size)
        .await
        .map_err(problem)?;
    respond(file, "image/webp", REVALIDATE, false, None, &headers).await
}

pub(super) async fn manifest(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let file = state
        .harness()
        .open_run_manifest(&parse_id(id)?)
        .await
        .map_err(problem)?;
    respond(file, "application/json", REVALIDATE, false, None, &headers).await
}

async fn respond(
    media: MediaFile,
    mime: &'static str,
    cache: &'static str,
    ranges: bool,
    range_headers: Option<&HeaderMap>,
    headers: &HeaderMap,
) -> Result<Response, Problem> {
    let etag = format!("\"{}\"", media.sha256);
    let mut response = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, cache);
    if ranges {
        response = response.header(header::ACCEPT_RANGES, "bytes");
    }
    let matched = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|value| value == "*" || value.strip_prefix("W/").unwrap_or(value) == etag);
    if matched {
        return Ok(response
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .expect("valid media headers"));
    }
    let mut file = tokio::fs::File::from_std(media.file);
    let mut len = media.len;
    if let Some(headers) = range_headers {
        let mut requested = headers.get_all(header::RANGE).iter();
        if let Some(value) = requested.next() {
            match value
                .to_str()
                .ok()
                .filter(|_| requested.next().is_none())
                .and_then(|value| byte_range(value, len))
            {
                Some((start, end)) => {
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(|error| problem(error.into()))?;
                    response = response
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
                    len = end - start + 1;
                }
                None => {
                    return Ok((
                        [
                            (header::CONTENT_RANGE, format!("bytes */{len}")),
                            (header::ACCEPT_RANGES, "bytes".into()),
                        ],
                        Problem::new(
                            StatusCode::RANGE_NOT_SATISFIABLE,
                            "The requested byte range cannot be served.",
                            "Send a single satisfiable bytes range or omit Range.",
                        ),
                    )
                        .into_response());
                }
            }
        }
    }
    if mime == "application/octet-stream" {
        response = response.header(header::CONTENT_DISPOSITION, "attachment");
    }
    Ok(response
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len)
        .body(Body::from_stream(ReaderStream::new(file.take(len))))
        .expect("valid media headers"))
}

fn byte_range(value: &str, len: u64) -> Option<(u64, u64)> {
    fn number(value: &str) -> Option<u64> {
        value
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| value.parse().ok())
            .flatten()
    }
    let last = len.checked_sub(1)?;
    let (unit, spec) = value.trim().split_once('=')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let suffix = number(end)?;
        return (suffix > 0).then_some((len.saturating_sub(suffix), last));
    }
    let start = number(start)?;
    let end = if end.is_empty() {
        last
    } else {
        number(end)?.min(last)
    };
    (start <= end).then_some((start, end))
}

fn content_type(filename: &str) -> &'static str {
    match mime_guess::from_path(filename).first_raw() {
        Some(
            media @ ("image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif"
            | "image/bmp" | "image/tiff" | "video/mp4" | "video/webm" | "video/quicktime"
            | "audio/mpeg" | "audio/wav" | "audio/x-wav" | "audio/ogg" | "audio/flac"),
        ) => media,
        _ => "application/octet-stream",
    }
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
            "Use an ID returned by the API.",
        )
    })
}

fn problem(error: HarnessError) -> Problem {
    let (status, recovery) = match &error {
        HarnessError::OutputNotFound => (StatusCode::NOT_FOUND, "Use an output ID returned by the outputs API."),
        HarnessError::RunNotFound { .. } | HarnessError::ManifestNotFound => (StatusCode::NOT_FOUND, "Use a run ID with a recorded completion manifest."),
        HarnessError::InvalidMediaRequest(_) => (StatusCode::BAD_REQUEST, "Check the media request parameters and try again."),
        HarnessError::ThumbnailDecode(_) => (StatusCode::UNPROCESSABLE_ENTITY, "Request the original output instead."),
        HarnessError::MediaIntegrity => (StatusCode::CONFLICT, "Check local run files against their recorded checksums; restore the original files from a trusted copy."),
        _ => {
            tracing::error!(%error, "Media API request failed");
            return Problem::new(StatusCode::INTERNAL_SERVER_ERROR, "The server could not read the stored media.", "Check local storage and the server logs.");
        }
    };
    Problem::new(status, error.to_string(), recovery)
}
