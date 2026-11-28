use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use genaimage_api as api;
use genaimage_core::{
    characters::{self as core, IdempotencyKey, Written},
    HarnessError,
};
use serde::{de::DeserializeOwned, Serialize};

use super::{
    outputs::output_response,
    runs::{self, core_status, idempotency_key, parse_query},
};
use crate::{AppState, Problem};

const NOT_FOUND: &str = "A named character, spec version, reference or output does not exist";
const WRITE_CONFLICT: &str =
    "A stale `based_on` (type `character-spec-conflict`, with `latest`), a conflicting state (`character-conflict`) or an idempotency key reused for another request (`idempotency-conflict`). Nothing was written";
const INVALID: &str =
    "The request is well-formed but not valid (`invalid-character-request`). Nothing was written";

#[utoipa::path(
    post, path = "/characters",
    operation_id = "characters_create",
    tag = "characters",
    description = "Create a character and, when `spec` is given, its version 1 in one transaction. The optional `Idempotency-Key` makes a retry return the character written the first time (200 instead of 201) and write nothing; the same key with another body, or on another endpoint, is a 409 `idempotency-conflict`.",
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::CreateCharacterRequest,
    responses(
        (status = 201, description = "Created", body = api::CreateCharacterResponse, headers(("Location" = String, description = "Character URL"))),
        (status = 200, description = "Idempotent replay of the first write", body = api::CreateCharacterResponse, headers(("Location" = String, description = "Character URL"))),
        (status = 409, description = WRITE_CONFLICT, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<api::CreateCharacterRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let key = optional_key(&headers)?;
    let request: core::NewCharacter = transcode(&json_body(body)?)?;
    let written = state
        .harness()
        .characters()
        .create_character_written(request, key.as_ref())
        .await
        .map_err(problem)?;
    let location = format!("/api/v1/characters/{}", written.record.character.id);
    created(written, Some(location.as_str()), &location, |created| {
        Ok(api::CreateCharacterResponse {
            character: transcode(&created.character)?,
            spec_version: created.spec_version.as_ref().map(transcode).transpose()?,
        })
    })
}

#[utoipa::path(
    get, path = "/characters",
    operation_id = "characters_list",
    tag = "characters",
    description = "List characters, newest first. A page without `next_cursor` is the last one.",
    params(api::CharacterListQuery),
    responses(
        (status = 200, body = api::CharacterListResponse),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
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
    query: Result<Query<api::CharacterListQuery>, QueryRejection>,
) -> Result<Json<api::CharacterListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_characters(core::CharacterListQuery {
            limit: query.limit,
            cursor: query.cursor,
        })
        .await
        .map_err(problem)?;
    Ok(Json(api::CharacterListResponse {
        characters: transcode(&page.characters)?,
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    get, path = "/characters/{id}",
    operation_id = "characters_get",
    tag = "characters",
    description = "Read a character and its latest spec version number.",
    params(("id" = String, Path, description = "Character ID")),
    responses(
        (status = 200, body = api::CharacterResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<api::CharacterResponse>, Problem> {
    let character = state
        .harness()
        .characters()
        .get_character(&character_id(id)?)
        .await
        .map_err(problem)?;
    Ok(Json(transcode(&character)?))
}

#[utoipa::path(
    get, path = "/characters/{id}/specs",
    operation_id = "characters_list_specs",
    tag = "characters",
    description = "List spec versions, newest first.",
    params(("id" = String, Path, description = "Character ID"), api::CharacterListQuery),
    responses(
        (status = 200, body = api::SpecVersionListResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_specs(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::CharacterListQuery>, QueryRejection>,
) -> Result<Json<api::SpecVersionListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_spec_versions(
            &character_id(id)?,
            core::SpecVersionListQuery {
                limit: query.limit,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(problem)?;
    Ok(Json(api::SpecVersionListResponse {
        versions: transcode(&page.versions)?,
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    post, path = "/characters/{id}/specs",
    operation_id = "characters_create_spec",
    tag = "characters",
    description = "Append a spec version. Versions are never edited. `based_on` must be the latest version the author saw (omitted for version 1); a stale value is a 409 `character-spec-conflict` carrying `latest`, and nothing is written. The serialised spec is limited to 64 KiB (422). The optional `Idempotency-Key` makes a retry return the version written the first time (200 instead of 201).",
    params(("id" = String, Path, description = "Character ID"), ("Idempotency-Key" = Option<String>, Header, description = "Optional key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::CreateSpecVersionRequest,
    responses(
        (status = 201, description = "Created", body = api::SpecVersionResponse, headers(("Location" = String, description = "Spec version URL"))),
        (status = 200, description = "Idempotent replay of the first write", body = api::SpecVersionResponse, headers(("Location" = String, description = "Spec version URL"))),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = WRITE_CONFLICT, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn create_spec(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<api::CreateSpecVersionRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let id = character_id(id)?;
    let key = optional_key(&headers)?;
    let request: core::NewSpecVersion = transcode(&json_body(body)?)?;
    let written = state
        .harness()
        .characters()
        .create_spec_version_written(&id, request, key.as_ref())
        .await
        .map_err(problem)?;
    let location = format!("/api/v1/characters/{id}/specs/{}", written.record.version);
    created(
        written,
        Some(location.as_str()),
        &location,
        transcode::<_, api::SpecVersionResponse>,
    )
}

#[utoipa::path(
    get, path = "/characters/{id}/specs/{version}",
    operation_id = "characters_get_spec",
    tag = "characters",
    description = "Read one spec version. `version` is a whole number without a sign or leading zeros.",
    params(("id" = String, Path, description = "Character ID"), ("version" = u32, Path, description = "Spec version")),
    responses(
        (status = 200, body = api::SpecVersionResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn get_spec(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<api::SpecVersionResponse>, Problem> {
    let (id, version) = path
        .map(|Path(path)| path)
        .map_err(|error| Problem::invalid_path(error, PATH_RECOVERY))?;
    let version = state
        .harness()
        .characters()
        .get_spec_version(&id, spec_version(&version)?)
        .await
        .map_err(problem)?;
    Ok(Json(transcode(&version)?))
}

#[utoipa::path(
    get, path = "/characters/{id}/references",
    operation_id = "characters_list_references",
    tag = "characters",
    description = "List references, newest first. Soft-removed references are listed only with `include_removed=true`.",
    params(("id" = String, Path, description = "Character ID"), api::ReferenceListQuery),
    responses(
        (status = 200, body = api::ReferenceListResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_references(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::ReferenceListQuery>, QueryRejection>,
) -> Result<Json<api::ReferenceListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_references(
            &character_id(id)?,
            core::ReferenceListQuery {
                include_removed: query.include_removed.unwrap_or(false),
                limit: query.limit,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(problem)?;
    Ok(Json(api::ReferenceListResponse {
        references: transcode(&page.references)?,
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    post, path = "/characters/{id}/references",
    operation_id = "characters_add_reference",
    tag = "characters",
    description = "Attach an existing upload with a role. An unknown upload is a 404 `character-upload-not-found`; attaching the same upload in the same role twice, or more than 32 active references, is refused. The optional `Idempotency-Key` makes a retry return the reference written the first time (200 instead of 201), even if the upload store has changed since.",
    params(("id" = String, Path, description = "Character ID"), ("Idempotency-Key" = Option<String>, Header, description = "Optional key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::AddReferenceRequest,
    responses(
        (status = 201, description = "Created", body = api::ReferenceResponse),
        (status = 200, description = "Idempotent replay of the first write", body = api::ReferenceResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = WRITE_CONFLICT, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn add_reference(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<api::AddReferenceRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let id = character_id(id)?;
    let key = optional_key(&headers)?;
    let request: core::NewReference = transcode(&json_body(body)?)?;
    let written = state
        .harness()
        .characters()
        .add_reference_written(&id, request, key.as_ref())
        .await
        .map_err(problem)?;
    let fetch = format!("/api/v1/characters/{id}/references?include_removed=true");
    created(
        written,
        None,
        &fetch,
        transcode::<_, api::ReferenceResponse>,
    )
}

#[utoipa::path(
    post, path = "/characters/{id}/references/{reference_id}/remove",
    operation_id = "characters_remove_reference",
    tag = "characters",
    description = "Soft-remove a reference: the record stays, with `removed_at` set once. Removing an already removed reference returns it unchanged. Send an empty body. Always 200: nothing new is created, so a replayed `Idempotency-Key` is indistinguishable from a first removal.",
    params(("id" = String, Path, description = "Character ID"), ("reference_id" = String, Path, description = "Reference ID"), ("Idempotency-Key" = Option<String>, Header, description = "Optional key, 1–128 visible ASCII characters excluding commas")),
    responses(
        (status = 200, body = api::ReferenceResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = WRITE_CONFLICT, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn remove_reference(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
    headers: HeaderMap,
) -> Result<Json<api::ReferenceResponse>, Problem> {
    let (id, reference_id) = path
        .map(|Path(path)| path)
        .map_err(|error| Problem::invalid_path(error, PATH_RECOVERY))?;
    let key = optional_key(&headers)?;
    let written = state
        .harness()
        .characters()
        .remove_reference_written(&id, &reference_id, key.as_ref())
        .await
        .map_err(problem)?;
    let fetch = format!("/api/v1/characters/{id}/references?include_removed=true");
    let reference = transcode(&written.record).map_err(|_| written_but_unreadable(&fetch))?;
    Ok(Json(reference))
}

#[utoipa::path(
    get, path = "/characters/{id}/decisions",
    operation_id = "characters_list_decisions",
    tag = "characters",
    description = "The append-only decision history, including `cleared` decisions, newest first. Filter by `output_id` (exactly `{run_id}:{row}`) and `spec_version`.",
    params(("id" = String, Path, description = "Character ID"), api::DecisionListQuery),
    responses(
        (status = 200, body = api::DecisionListResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_decisions(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::DecisionListQuery>, QueryRejection>,
) -> Result<Json<api::DecisionListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_decisions(
            &character_id(id)?,
            core::DecisionListQuery {
                output_id: query.output_id,
                spec_version: query.spec_version,
                limit: query.limit,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(problem)?;
    Ok(Json(api::DecisionListResponse {
        decisions: transcode(&page.decisions)?,
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    post, path = "/characters/{id}/decisions",
    operation_id = "characters_record_decision",
    tag = "characters",
    description = "Append an approve, reject or clear decision about an output against a spec version. Decisions are never edited; `cleared` undoes the current one. The output must exist (404), be exactly `{run_id}:{row}` (422), and to be approved or rejected belong to a completed run (422); an output generated for another character, or `cleared` without a current decision, is a 409 `character-conflict`. The optional `Idempotency-Key` makes a retry return the decision written the first time (200 instead of 201).",
    params(("id" = String, Path, description = "Character ID"), ("Idempotency-Key" = Option<String>, Header, description = "Optional key, 1–128 visible ASCII characters excluding commas")),
    request_body = api::RecordDecisionRequest,
    responses(
        (status = 201, description = "Recorded", body = api::DecisionResponse),
        (status = 200, description = "Idempotent replay of the first write", body = api::DecisionResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = WRITE_CONFLICT, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 415, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn record_decision(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<api::RecordDecisionRequest>, JsonRejection>,
) -> Result<impl IntoResponse, Problem> {
    let id = character_id(id)?;
    let key = optional_key(&headers)?;
    let request: core::NewDecision = transcode(&json_body(body)?)?;
    let written = state
        .harness()
        .characters()
        .record_decision_written(&id, request, key.as_ref())
        .await
        .map_err(problem)?;
    let fetch = format!("/api/v1/characters/{id}/decisions");
    created(written, None, &fetch, transcode::<_, api::DecisionResponse>)
}

#[utoipa::path(
    get, path = "/characters/{id}/outputs",
    operation_id = "characters_list_outputs",
    tag = "characters",
    description = "Current decision states derived from the history (the latest decision per output, `cleared` ones omitted), each joined with its output, newest decision first. `state=approved` or `state=rejected` narrows them; omit it for both.",
    params(("id" = String, Path, description = "Character ID"), api::CharacterOutputsQuery),
    responses(
        (status = 200, body = api::CharacterOutputListResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_outputs(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::CharacterOutputsQuery>, QueryRejection>,
) -> Result<Json<api::CharacterOutputListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_current_states_with_outputs(
            &character_id(id)?,
            core::CurrentStateQuery {
                state: query.state.map(|state| match state {
                    api::CurrentDecisionState::Approved => core::Decision::Approved,
                    api::CurrentDecisionState::Rejected => core::Decision::Rejected,
                }),
                spec_version: query.spec_version,
                limit: query.limit,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(problem)?;
    let outputs = page
        .decisions
        .into_iter()
        .map(|row| {
            Ok(api::CharacterOutputResponse {
                decision: transcode(&row.decision)?,
                output: output_response(row.output),
            })
        })
        .collect::<Result<_, Problem>>()?;
    Ok(Json(api::CharacterOutputListResponse {
        outputs,
        next_cursor: page.next_cursor,
    }))
}

#[utoipa::path(
    get, path = "/characters/{id}/runs",
    operation_id = "characters_list_runs",
    tag = "characters",
    description = "Runs submitted against the character, newest first. Filter by `spec_version` and `status`.",
    params(("id" = String, Path, description = "Character ID"), api::CharacterRunsQuery),
    responses(
        (status = 200, body = api::RunListResponse),
        (status = 404, description = NOT_FOUND, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = INVALID, body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 400, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn list_runs(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<api::CharacterRunsQuery>, QueryRejection>,
) -> Result<Json<api::RunListResponse>, Problem> {
    let query = parse_query(query)?;
    let page = state
        .harness()
        .characters()
        .list_runs(
            &character_id(id)?,
            core::CharacterRunQuery {
                spec_version: query.spec_version,
                status: query.status.map(core_status),
                limit: query.limit,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(problem)?;
    Ok(Json(api::RunListResponse {
        runs: page.runs.into_iter().map(runs::run_response).collect(),
        next_cursor: page.next_cursor,
    }))
}

const PATH_RECOVERY: &str = "Use the IDs and version numbers returned by the characters API.";

fn character_id(id: Result<Path<String>, PathRejection>) -> Result<String, Problem> {
    id.map(|Path(id)| id)
        .map_err(|error| Problem::invalid_path(error, PATH_RECOVERY))
}

/// A whole number: digits only, no sign and no leading zero, so one version has one URL.
fn spec_version(text: &str) -> Result<u32, Problem> {
    let canonical = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    canonical
        .then(|| text.parse().ok())
        .flatten()
        .ok_or_else(|| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                format!("'{text}' is not a spec version number."),
                "Use a whole number such as 1, as listed by the character's specs.",
            )
        })
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Problem> {
    body.map(|Json(body)| body).map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send application/json that matches the documented request. Nothing was written.",
        )
    })
}

/// Absent is fine; present must be one valid key, as for runs.
fn optional_key(headers: &HeaderMap) -> Result<Option<IdempotencyKey>, Problem> {
    if !headers.contains_key("idempotency-key") {
        return Ok(None);
    }
    IdempotencyKey::parse(&idempotency_key(headers)?)
        .map(Some)
        .map_err(problem)
}

/// 201, or 200 when the key replayed the first write. `fetch` is where to re-read the record.
fn created<T, R: Serialize>(
    written: Written<T>,
    location: Option<&str>,
    fetch: &str,
    convert: impl FnOnce(&T) -> Result<R, Problem>,
) -> Result<impl IntoResponse, Problem> {
    let status = if written.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let mut headers = HeaderMap::new();
    if let Some(location) = location.and_then(|location| HeaderValue::from_str(location).ok()) {
        headers.insert(header::LOCATION, location);
    }
    let body = convert(&written.record).map_err(|_| written_but_unreadable(fetch))?;
    Ok((status, headers, Json(body)))
}

/// The write has committed, so a failed read-back must not look like a failed write.
fn written_but_unreadable(fetch: &str) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The record was written, but the server could not build its response.",
        format!(
            "The record was written; fetch it again with GET {fetch}. Do not repeat the write."
        ),
    )
}

/// The API and core types share one JSON shape; this converts between them.
fn transcode<T: Serialize, U: DeserializeOwned>(value: &T) -> Result<U, Problem> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|error| {
            tracing::error!(%error, "Character API type no longer matches the core type");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not convert a character record.",
                "Check the server logs.",
            )
        })
}

fn problem(error: HarnessError) -> Problem {
    let (status, kind, recovery) = match &error {
        HarnessError::CharacterNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "character-not-found",
            "Use a character ID returned by the characters API.",
        ),
        HarnessError::CharacterSpecVersionNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "character-spec-version-not-found",
            "Use a version number listed by the character's specs.",
        ),
        HarnessError::CharacterReferenceNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "character-reference-not-found",
            "Use a reference ID listed by the character's references.",
        ),
        HarnessError::CharacterUploadNotFound { .. } => (
            StatusCode::NOT_FOUND,
            "character-upload-not-found",
            "Upload the image with POST /api/v1/uploads and attach the returned ID. Nothing was written.",
        ),
        HarnessError::OutputNotFound => (
            StatusCode::NOT_FOUND,
            "output-not-found",
            "Use an output ID returned by the outputs API. Nothing was written.",
        ),
        HarnessError::CharacterSpecConflict { latest, .. } => {
            return Problem::new(
                StatusCode::CONFLICT,
                error.to_string(),
                "Read the character's latest spec version, review it and base the new version on it. Nothing was written.",
            )
            .with_type("urn:genaimage:problem:character-spec-conflict")
            .with_latest(*latest);
        }
        HarnessError::CharacterConflict(_) => (
            StatusCode::CONFLICT,
            "character-conflict",
            "Review the character's current state before changing it. Nothing was written.",
        ),
        HarnessError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "idempotency-conflict",
            "Repeat the original request with this key, or use a fresh key for a different request. Nothing new was written.",
        ),
        HarnessError::InvalidCharacterRequest(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-character-request",
            "Correct the request and send it again. Nothing was written.",
        ),
        HarnessError::ShuttingDown => return runs::problem(error),
        _ => {
            tracing::error!(%error, "Characters API request failed");
            return Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not complete the character operation.",
                "Check the server logs, then re-read the character before retrying; each write is one transaction, so a failed one leaves nothing behind.",
            );
        }
    };
    Problem::new(status, error.to_string(), recovery)
        .with_type(&format!("urn:genaimage:problem:{kind}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_error_has_a_client_status_and_a_typed_problem() {
        let missing = || "char-1".to_owned();
        let errors = [
            HarnessError::CharacterNotFound {
                character_id: missing(),
            },
            HarnessError::CharacterSpecVersionNotFound {
                character_id: missing(),
                version: 2,
            },
            HarnessError::CharacterReferenceNotFound {
                character_id: missing(),
                reference_id: "charref-1".into(),
            },
            HarnessError::CharacterUploadNotFound {
                upload_id: "sha256:x".into(),
            },
            HarnessError::OutputNotFound,
            HarnessError::CharacterSpecConflict {
                character_id: missing(),
                based_on: Some(1),
                latest: Some(3),
            },
            HarnessError::CharacterConflict("conflict".into()),
            HarnessError::IdempotencyConflict,
            HarnessError::InvalidCharacterRequest("invalid".into()),
        ];
        for error in errors {
            let name = format!("{error:?}");
            let response = problem(error).into_response();
            assert!(
                response.status().is_client_error(),
                "{name} gave {}",
                response.status()
            );
        }
    }

    #[tokio::test]
    async fn a_failed_read_back_after_a_write_says_the_write_happened() {
        let response = written_but_unreadable("/api/v1/characters/char-1").into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains(
            "The record was written; fetch it again with GET /api/v1/characters/char-1."
        ));
    }

    #[test]
    fn spec_versions_are_whole_numbers_without_aliases() {
        assert_eq!(spec_version("0").ok(), Some(0));
        assert_eq!(spec_version("12").ok(), Some(12));
        for text in ["", "01", "+1", "-1", "1.0", "1e0", " 1", "4294967296"] {
            assert!(spec_version(text).is_err(), "{text:?}");
        }
    }
}
