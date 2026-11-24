use super::BinaryData;
use axum::{
    body::Body,
    extract::{
        rejection::{PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
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

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct OutputStatusQuery {
    /// Exact run status. Defaults to completed; only terminal runs can expose indexed outputs.
    status: Option<api::RunStatus>,
}

#[utoipa::path(
    get, path = "/outputs",
    operation_id = "outputs_list",
    tag = "outputs",
    description = "List indexed outputs.",
    params(api::OutputListQuery, OutputStatusQuery),
    responses(
        (status = 200, body = api::OutputListResponse),
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
    query: Result<Query<api::OutputListQuery>, QueryRejection>,
    status: Result<Query<OutputStatusQuery>, QueryRejection>,
) -> Result<Json<api::OutputListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .repository()
        .as_async()
        .list_outputs(OutputListQuery {
            status: parse_query(status)?.status.map(super::runs::core_status),
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
        outputs: page.outputs.into_iter().map(output_response).collect(),
        next_cursor: page.next_cursor,
    }))
}

fn output_response(indexed: genaimage_core::media::IndexedOutput) -> api::OutputResponse {
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
}

#[utoipa::path(
    get, path = "/outputs/{id}/metadata",
    operation_id = "outputs_metadata",
    tag = "outputs",
    description = "Fetch indexed metadata without transferring the original bytes.",
    params(("id" = String, Path, description = "Output ID")),
    responses(
        (status = 200, body = api::OutputResponse),
        (status = 404, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn metadata(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<api::OutputResponse>, Problem> {
    let id = parse_id(path)?;
    state
        .harness()
        .repository()
        .as_async()
        .output(&id)
        .await
        .map(|output| Json(output_response(output)))
        .map_err(problem)
}

#[utoipa::path(
    get, path = "/outputs/{id}",
    operation_id = "outputs_original",
    tag = "outputs",
    description = "Fetch an original output with conditional and range requests.",
    params(("id" = String, Path, description = "Output ID"), ("If-None-Match" = Option<String>, Header), ("Range" = Option<String>, Header, description = "Single bytes range"), ("If-Range" = Option<String>, Header)),
    responses(
        (status = 200, description = "Original output bytes", content((BinaryData = "image/png"), (BinaryData = "image/jpeg"), (BinaryData = "image/gif"), (BinaryData = "image/webp"), (BinaryData = "image/avif"), (BinaryData = "image/bmp"), (BinaryData = "image/tiff"), (BinaryData = "video/mp4"), (BinaryData = "video/webm"), (BinaryData = "video/quicktime"), (BinaryData = "audio/mpeg"), (BinaryData = "audio/wav"), (BinaryData = "audio/x-wav"), (BinaryData = "audio/ogg"), (BinaryData = "audio/flac"), (BinaryData = "application/octet-stream")), headers(("ETag" = String), ("Cache-Control" = String), ("Accept-Ranges" = String))), (status = 206, description = "Partial output bytes", content((BinaryData = "image/png"), (BinaryData = "image/jpeg"), (BinaryData = "image/gif"), (BinaryData = "image/webp"), (BinaryData = "image/avif"), (BinaryData = "image/bmp"), (BinaryData = "image/tiff"), (BinaryData = "video/mp4"), (BinaryData = "video/webm"), (BinaryData = "video/quicktime"), (BinaryData = "audio/mpeg"), (BinaryData = "audio/wav"), (BinaryData = "audio/x-wav"), (BinaryData = "audio/ogg"), (BinaryData = "audio/flac"), (BinaryData = "application/octet-stream")), headers(("Content-Range" = String), ("ETag" = String))), (status = 304, description = "Not modified", headers(("ETag" = String))), (status = 416, description = "Range not satisfiable", body = genaimage_api::ProblemDetails, content_type = "application/problem+json", headers(("Content-Range" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn original(
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

#[utoipa::path(
    get, path = "/outputs/{id}/thumbnail",
    operation_id = "outputs_thumbnail",
    tag = "outputs",
    description = "Fetch a WebP thumbnail.",
    params(("id" = String, Path, description = "Output ID"), api::ThumbnailQuery, ("If-None-Match" = Option<String>, Header)),
    responses(
        (status = 200, body = BinaryData, content_type = "image/webp", headers(("ETag" = String), ("Cache-Control" = String))), (status = 304, description = "Not modified", headers(("ETag" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn thumbnail(
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

#[utoipa::path(
    get, path = "/runs/{id}/resolved-workflow",
    operation_id = "outputs_resolved_workflow",
    tag = "outputs",
    description = "Fetch the archived resolved workflow. New runs verify its SHA-256 against the pre-submission record; legacy archives are unverified.",
    params(("id" = String, Path, description = "Run ID"), ("If-None-Match" = Option<String>, Header)),
    responses(
        (status = 200, description = "Archived resolved workflow", body = Object, headers(("ETag" = String), ("X-Genaimage-Workflow-Integrity" = String, description = "verified or unverified"))),
        (status = 304, description = "Not modified", headers(("ETag" = String), ("X-Genaimage-Workflow-Integrity" = String, description = "verified or unverified"))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn resolved_workflow(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let file = state
        .harness()
        .open_resolved_workflow(&parse_id(id)?)
        .await
        .map_err(workflow_problem)?;
    let integrity = if file.integrity_verified {
        "verified"
    } else {
        "unverified"
    };
    let mut response = respond(
        file.media,
        "application/json",
        REVALIDATE,
        false,
        None,
        &headers,
    )
    .await?;
    response.headers_mut().insert(
        "x-genaimage-workflow-integrity",
        axum::http::HeaderValue::from_static(integrity),
    );
    Ok(response)
}

#[utoipa::path(
    get, path = "/runs/{id}/manifest",
    operation_id = "outputs_manifest",
    tag = "outputs",
    description = "Fetch the preserved run manifest.",
    params(("id" = String, Path, description = "Run ID"), ("If-None-Match" = Option<String>, Header)),
    responses(
        (status = 200, description = "Archived run manifest JSON", body = Object, headers(("ETag" = String), ("X-Genaimage-Manifest-Integrity" = String, description = "unverified when integrity could not be verified"))), (status = 304, description = "Not modified", headers(("ETag" = String))),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn manifest(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let manifest = state
        .harness()
        .open_run_manifest(&parse_id(id)?)
        .await
        .map_err(problem)?;
    let mut response = respond(
        manifest.media,
        "application/json",
        REVALIDATE,
        false,
        None,
        &headers,
    )
    .await?;
    if !manifest.integrity_verified {
        response.headers_mut().insert(
            "x-genaimage-manifest-integrity",
            axum::http::HeaderValue::from_static("unverified"),
        );
    }
    Ok(response)
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
    if if_none_match(headers, etag.as_bytes()) {
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
            let range = value
                .to_str()
                .ok()
                .filter(|_| requested.next().is_none())
                .map_or(ByteRange::Unsatisfiable, |value| byte_range(value, len));
            match range {
                ByteRange::Ignore => {}
                ByteRange::Satisfiable(start, end) => {
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(|error| problem(error.into()))?;
                    response = response
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
                    len = end - start + 1;
                }
                ByteRange::Unsatisfiable => {
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

fn trim_ows(value: &[u8]) -> &[u8] {
    let is_ows = |byte: &u8| matches!(byte, b' ' | b'\t');
    let Some(start) = value.iter().position(|byte| !is_ows(byte)) else {
        return &[];
    };
    let end = value.iter().rposition(|byte| !is_ows(byte)).unwrap();
    &value[start..=end]
}

fn if_none_match(headers: &HeaderMap, etag: &[u8]) -> bool {
    let fields = headers.get_all(header::IF_NONE_MATCH);
    let mut iter = fields.iter();
    if let Some(first) = iter.next() {
        if trim_ows(first.as_bytes()) == b"*" && iter.next().is_none() {
            return true;
        }
    }
    let mut matched = false;
    for value in fields.iter() {
        let mut rest = trim_ows(value.as_bytes());
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix(b",") {
                rest = trim_ows(after);
                continue;
            }
            let tag = rest.strip_prefix(b"W/").unwrap_or(rest);
            let Some(opaque) = tag.strip_prefix(b"\"") else {
                return false;
            };
            let Some(end) = opaque.iter().position(|&b| b == b'"') else {
                return false;
            };
            if !opaque[..end]
                .iter()
                .all(|&b| matches!(b, b'!' | b'#'..=b'~' | 0x80..=0xff))
            {
                return false;
            }
            matched |= &tag[..end + 2] == etag;
            let after = trim_ows(&opaque[end + 1..]);
            if after.is_empty() {
                break;
            }
            let Some(after) = after.strip_prefix(b",") else {
                return false;
            };
            rest = trim_ows(after);
        }
    }
    matched
}

enum ByteRange {
    Ignore,
    Unsatisfiable,
    Satisfiable(u64, u64),
}

fn byte_range(value: &str, len: u64) -> ByteRange {
    let Some((unit, spec)) = value.trim().split_once('=') else {
        return ByteRange::Unsatisfiable;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return ByteRange::Ignore;
    }
    match parse_byte_spec(spec, len) {
        Some((start, end)) => ByteRange::Satisfiable(start, end),
        None => ByteRange::Unsatisfiable,
    }
}

fn parse_byte_spec(spec: &str, len: u64) -> Option<(u64, u64)> {
    fn number(value: &str) -> Option<u64> {
        if value.is_empty() {
            return None;
        }
        value.bytes().try_fold(0_u64, |acc, digit| {
            digit.is_ascii_digit().then(|| {
                acc.saturating_mul(10)
                    .saturating_add(u64::from(digit - b'0'))
            })
        })
    }
    let last = len.checked_sub(1)?;
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
    id.map(|Path(id)| id)
        .map_err(|error| Problem::invalid_path(error, "Use an ID returned by the API."))
}

fn workflow_problem(error: HarnessError) -> Problem {
    match error {
        HarnessError::RunNotFound { .. } => Problem::new(
            StatusCode::NOT_FOUND,
            error.to_string(),
            "Use a run ID returned by the runs API.",
        ),
        HarnessError::MediaIntegrity => Problem::new(
            StatusCode::CONFLICT,
            "The archived resolved workflow or its submission record could not be verified.",
            "Restore the original resolved workflow and submission record from a trusted copy.",
        ),
        _ => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The server could not read the archived resolved workflow.",
            "Check local run storage and the server logs.",
        ),
    }
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
