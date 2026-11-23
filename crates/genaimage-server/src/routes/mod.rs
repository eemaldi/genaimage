mod events;
mod health;
mod outputs;
mod runs;
mod settings;
mod uploads;
mod workflows;

use axum::{http::StatusCode, routing::get, Json, Router};
use utoipa::{OpenApi, ToSchema};

struct BinaryData;
impl utoipa::PartialSchema for BinaryData {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                utoipa::openapi::KnownFormat::Binary,
            )))
            .into()
    }
}
impl ToSchema for BinaryData {}

#[derive(ToSchema)]
#[allow(dead_code)]
struct UploadBody {
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        health::health,
        events::events,
        workflows::list,
        workflows::details,
        workflows::validate,
        workflows::thumbnail,
        runs::list,
        runs::submit,
        runs::get_run,
        runs::events,
        runs::cancel,
        runs::reconcile,
        runs::reconcile_all,
        outputs::manifest,
        outputs::list,
        outputs::original,
        outputs::thumbnail,
        uploads::upload,
        uploads::metadata,
        settings::read,
        settings::save,
        settings::probe,
        openapi_endpoint
    ),
    components(schemas(genaimage_api::EventEnvelope, genaimage_api::ResyncEvent))
)]
struct ApiDoc;

/// Returns the generated OpenAPI 3.1 contract.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut document = ApiDoc::openapi();
    // Axum's GET routes also handle HEAD, with the response body suppressed.
    for (path, item) in &mut document.paths.paths {
        if let Some(get) = &item.get {
            let mut head = get.clone();
            head.operation_id = head.operation_id.map(|id| format!("{id}_head"));
            for response in head.responses.responses.values_mut() {
                if let utoipa::openapi::RefOr::T(response) = response {
                    response.content.clear();
                }
            }
            if path == "/api/v1/outputs/{id}" {
                head.responses.responses.remove("206");
                head.responses.responses.remove("416");
                if let Some(params) = &mut head.parameters {
                    params.retain(|param| param.name != "Range" && param.name != "If-Range");
                }
            }
            item.head = Some(head);
        }
    }
    document
}

#[utoipa::path(get, path = "/api/v1/openapi.json",
    operation_id = "mod_openapi_endpoint", responses(
    (status = 200, description = "OpenAPI 3.1 document", body = Object),
    (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
    (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
    (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
    (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
    (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
    (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
))]
async fn openapi_endpoint() -> Json<utoipa::openapi::OpenApi> {
    Json(openapi())
}

use crate::{AppState, Problem};

pub(crate) fn api() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_endpoint))
        .merge(health::routes())
        .merge(outputs::routes())
        .merge(events::routes())
        .merge(runs::routes())
        .merge(settings::routes())
        .merge(uploads::routes())
        .merge(workflows::routes())
        .fallback(|| async {
            Problem::new(
                StatusCode::NOT_FOUND,
                "No API endpoint matches this path.",
                "Check the path and API version.",
            )
        })
        .method_not_allowed_fallback(|| async {
            Problem::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "This endpoint does not support the request method.",
                "Use a method listed in the Allow response header.",
            )
        })
}
