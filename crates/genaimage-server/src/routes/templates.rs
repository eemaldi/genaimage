use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path as AxumPath, Query, State,
    },
    http::StatusCode,
    Json,
};
use genaimage_api::{
    SettingError, TemplateCreateRequest, TemplateDeleteQuery, TemplateDiagnostic,
    TemplateListResponse, TemplateRenderRequest, TemplateRenderResponse, TemplateReplaceRequest,
    TemplateScope, TemplateVariable, TemplateVariableInput,
};
use genaimage_core::templates::{
    self, TemplateDefinition, TemplateError, TemplateIssue, VariableDefinition,
};
use serde_json::Map;

use crate::{AppState, Problem};

fn internal_error(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
        "Check the template directory and server logs.",
    )
}

fn errors(issues: Vec<TemplateIssue>) -> Vec<SettingError> {
    issues
        .into_iter()
        .map(|issue| SettingError {
            key: issue.key,
            reason: issue.reason,
        })
        .collect()
}

fn template_error(error: TemplateError) -> Problem {
    let detail = error.to_string();
    match error {
        TemplateError::InvalidRef(_) => Problem::new(
            StatusCode::BAD_REQUEST,
            detail,
            "Use a ref from the template listing.",
        )
        .with_type("urn:genaimage:problem:template-ref-invalid"),
        TemplateError::NotFound(_) => Problem::new(
            StatusCode::NOT_FOUND,
            detail,
            "Reload the template listing; the template may have been deleted.",
        )
        .with_type("urn:genaimage:problem:template-not-found"),
        TemplateError::Invalid(issues) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            detail,
            "Fix the listed fields and send the request again. Nothing is corrected automatically.",
        )
        .with_type("urn:genaimage:problem:template-invalid")
        .with_errors(errors(issues)),
        TemplateError::Exists(_) => Problem::new(
            StatusCode::CONFLICT,
            detail,
            "Choose another id. Existing templates are never overwritten by a create; replace one with PUT and its sha256.",
        )
        .with_type("urn:genaimage:problem:template-exists"),
        TemplateError::Changed { current_sha256 } => Problem::new(
            StatusCode::CONFLICT,
            "The template changed since you reviewed it; nothing was written or rendered.",
            "Reload the template, review it again and retry with the new sha256.",
        )
        .with_type("urn:genaimage:problem:template-changed")
        .with_current_sha256(current_sha256),
        TemplateError::RenderInvalid(issues) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            detail,
            "Supply every required variable once, as a string, and no variable the template does not declare.",
        )
        .with_type("urn:genaimage:problem:template-render-invalid")
        .with_errors(errors(issues)),
        TemplateError::Io(_) => internal_error(detail),
    }
}

fn body_rejection(error: JsonRejection) -> Problem {
    Problem::new(
        error.status(),
        error.body_text(),
        "Send a JSON object as documented for this endpoint.",
    )
}

fn path_rejection(error: PathRejection) -> Problem {
    Problem::invalid_path(error, "Use a ref from the template listing.")
}

fn api_template(template: templates::Template) -> genaimage_api::Template {
    genaimage_api::Template {
        reference: template.reference,
        scope: match template.scope {
            templates::TemplateScope::User => TemplateScope::User,
            templates::TemplateScope::Pack => TemplateScope::Pack,
        },
        pack_id: template.pack_id,
        id: template.id,
        title: template.title,
        description: template.description,
        variables: template
            .variables
            .into_iter()
            .map(|variable| TemplateVariable {
                name: variable.name,
                description: variable.description,
                required: variable.required,
                default: variable.default,
            })
            .collect(),
        sha256: template.sha256,
        text: template.text,
    }
}

fn definition(
    id: String,
    title: String,
    description: Option<String>,
    text: String,
    variables: std::collections::BTreeMap<String, TemplateVariableInput>,
) -> TemplateDefinition {
    TemplateDefinition {
        id,
        title,
        description,
        text,
        variables: variables
            .into_iter()
            .map(|(name, variable)| {
                (
                    name,
                    VariableDefinition {
                        description: variable.description,
                        required: variable.required,
                        default: variable.default,
                    },
                )
            })
            .collect(),
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TemplateError> + Send + 'static,
) -> Result<T, Problem> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(internal_error)?
        .map_err(template_error)
}

#[utoipa::path(
    get, path = "/templates",
    operation_id = "templates_list",
    tag = "templates",
    description = "List prompt templates: user templates from `paths.templates` and read-only templates declared by packs. Bounded to 256 templates and 100 diagnostics. A template that is invalid appears only in `diagnostics`, never in `templates`. Each template carries its full `text` and the `sha256` of its file.",
    responses(
        (status = 200, body = TemplateListResponse),
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
) -> Result<Json<TemplateListResponse>, Problem> {
    let harness = state.harness();
    let listing = tokio::task::spawn_blocking(move || harness.list_templates())
        .await
        .map_err(internal_error)?;
    Ok(Json(TemplateListResponse {
        templates: listing.templates.into_iter().map(api_template).collect(),
        diagnostics: listing
            .diagnostics
            .into_iter()
            .map(|item| TemplateDiagnostic {
                reference: item.reference,
                path: item.path,
                error: item.error,
            })
            .collect(),
    }))
}

#[utoipa::path(
    get, path = "/templates/user/{id}",
    operation_id = "templates_get_user",
    tag = "templates",
    description = "One user template, ref `user:<id>`.",
    params(("id" = String, Path, description = "Template id")),
    responses(
        (status = 200, body = genaimage_api::Template),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "`template-not-found`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn get_user(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
) -> Result<Json<genaimage_api::Template>, Problem> {
    let AxumPath(id) = id.map_err(path_rejection)?;
    let harness = state.harness();
    let template = blocking(move || harness.template(&templates::TemplateRef::user(&id))).await?;
    Ok(Json(api_template(template)))
}

#[utoipa::path(
    get, path = "/templates/pack/{pack_id}/{path}",
    operation_id = "templates_get_pack",
    tag = "templates",
    description = "One read-only pack template, ref `pack:<pack_id>:<path>`. `path` is the relative path declared in `pack.toml` as one percent-encoded segment (`/` as `%2F`). It is matched against the declared list and never used as a file name. PUT and DELETE answer 405.",
    params(
        ("pack_id" = String, Path, description = "Pack id"),
        ("path" = String, Path, description = "Template path relative to the pack, as declared by the pack")
    ),
    responses(
        (status = 200, body = genaimage_api::Template),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "`template-not-found`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn get_pack(
    State(state): State<AppState>,
    params: Result<AxumPath<(String, String)>, PathRejection>,
) -> Result<Json<genaimage_api::Template>, Problem> {
    let AxumPath((pack_id, path)) = params.map_err(path_rejection)?;
    let harness = state.harness();
    let template =
        blocking(move || harness.template(&templates::TemplateRef::pack(&pack_id, &path))).await?;
    Ok(Json(api_template(template)))
}

#[utoipa::path(
    post, path = "/templates",
    operation_id = "templates_create",
    tag = "templates",
    description = "Create a user template as `<paths.templates>/<id>.toml`, creating the directory on the first write. Never overwrites: an existing id answers 409 `urn:genaimage:problem:template-exists` and writes nothing. An invalid template answers 422 `urn:genaimage:problem:template-invalid` with `errors[{key, reason}]`. Responds 201 with the stored template.",
    request_body = TemplateCreateRequest,
    responses(
        (status = 201, body = genaimage_api::Template),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "`template-exists` or `template-changed`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn create(
    State(state): State<AppState>,
    request: Result<Json<TemplateCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<genaimage_api::Template>), Problem> {
    let Json(request) = request.map_err(body_rejection)?;
    let harness = state.harness();
    let template = blocking(move || {
        harness.create_template(&definition(
            request.id,
            request.title,
            request.description,
            request.text,
            request.variables,
        ))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(api_template(template))))
}

#[utoipa::path(
    put, path = "/templates/user/{id}",
    operation_id = "templates_replace",
    tag = "templates",
    description = "Replace a user template atomically. `expected_sha256` must equal the sha256 of the file as it is now, checked under the same lock as the write; otherwise 409 `urn:genaimage:problem:template-changed` with `current_sha256`, and the file is untouched. The id cannot change.",
    params(("id" = String, Path, description = "Template id")),
    request_body = TemplateReplaceRequest,
    responses(
        (status = 200, body = genaimage_api::Template),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "`template-not-found`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "`template-exists` or `template-changed`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn replace(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
    request: Result<Json<TemplateReplaceRequest>, JsonRejection>,
) -> Result<Json<genaimage_api::Template>, Problem> {
    let AxumPath(id) = id.map_err(path_rejection)?;
    let Json(request) = request.map_err(body_rejection)?;
    let harness = state.harness();
    let template = blocking(move || {
        let definition = definition(
            id.clone(),
            request.title,
            request.description,
            request.text,
            request.variables,
        );
        harness.replace_template(&id, &request.expected_sha256, &definition)
    })
    .await?;
    Ok(Json(api_template(template)))
}

#[utoipa::path(
    delete, path = "/templates/user/{id}",
    operation_id = "templates_delete",
    tag = "templates",
    description = "Delete a user template. The query parameter `expected_sha256` must equal the sha256 of the file as it is now, checked under the same lock; otherwise 409 `urn:genaimage:problem:template-changed`. Responds 204.",
    params(
        ("id" = String, Path, description = "Template id"),
        ("expected_sha256" = String, Query, description = "The sha256 of the version being deleted")
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "`template-not-found`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "`template-exists` or `template-changed`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    id: Result<AxumPath<String>, PathRejection>,
    query: Result<Query<TemplateDeleteQuery>, QueryRejection>,
) -> Result<StatusCode, Problem> {
    let AxumPath(id) = id.map_err(path_rejection)?;
    let Query(query) = query.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send the sha256 of the version you reviewed as ?expected_sha256=.",
        )
    })?;
    let harness = state.harness();
    blocking(move || harness.delete_template(&id, &query.expected_sha256)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/templates/render",
    operation_id = "templates_render",
    tag = "templates",
    description = "Render a template to prompt text. Pure and deterministic: no trimming, no normalisation, no LLM. `sha256` must equal the sha256 of the template as it is now, otherwise 409 `urn:genaimage:problem:template-changed`. `variables` maps names to strings; absent variables take their default and an empty string is a value. A missing required variable, an unknown variable or a non-string value answers 422 `urn:genaimage:problem:template-render-invalid` with `errors[{key, reason}]`. Rendering does not submit or record anything.",
    request_body = TemplateRenderRequest,
    responses(
        (status = 200, body = TemplateRenderResponse),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 404, description = "`template-not-found`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 409, description = "`template-exists` or `template-changed`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 422, description = "`template-invalid` or `template-render-invalid`", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),

    )
)]
pub(super) async fn render(
    State(state): State<AppState>,
    request: Result<Json<TemplateRenderRequest>, JsonRejection>,
) -> Result<Json<TemplateRenderResponse>, Problem> {
    let Json(request) = request.map_err(body_rejection)?;
    let harness = state.harness();
    let rendered = blocking(move || {
        let variables: Map<String, serde_json::Value> = request.variables.into_iter().collect();
        harness.render_template(&request.reference, &request.sha256, &variables)
    })
    .await?;
    Ok(Json(TemplateRenderResponse {
        text: rendered.text,
        reference: rendered.reference,
        sha256: rendered.sha256,
    }))
}
