mod batches;
pub(crate) mod conversations;
mod doctor;
mod events;
mod health;
pub(crate) mod models;
mod outputs;
mod packs;
pub(crate) mod prompt;
mod runs;
mod settings;
mod setup;
mod suggestions;
mod uploads;
pub(crate) mod workflows;

use axum::{http::StatusCode, Json, Router};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

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
#[openapi(components(schemas(
    genaimage_api::AssistantAwaitingConfirmationEvent,
    genaimage_api::AssistantDeltaEvent,
    genaimage_api::AssistantMessageStartedEvent,
    genaimage_api::AssistantTerminalEvent,
    genaimage_api::AssistantToolCallEvent,
    genaimage_api::AssistantToolResultEvent,
    genaimage_api::AssistantConfirmationRequiredEvent,
    genaimage_api::EventEnvelope,
    genaimage_api::ResyncEvent,
    genaimage_api::ThumbnailSizeSchema
)))]
struct ApiDoc;

/// Returns the generated OpenAPI 3.1 contract.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let (_, mut document) = router_and_openapi();
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

#[utoipa::path(get, path = "/openapi.json",
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

// Register handlers only through routes! so routing and the contract stay coupled.
fn api() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        .routes(routes!(openapi_endpoint))
        .routes(routes!(health::health))
        .routes(routes!(doctor::doctor))
        .routes(routes!(events::events))
        .routes(routes!(conversations::list, conversations::create))
        .routes(routes!(suggestions::suggest))
        .routes(routes!(conversations::get))
        .routes(routes!(conversations::send_message))
        .routes(routes!(conversations::preview_confirmation))
        .routes(routes!(conversations::decide_confirmation))
        .routes(routes!(conversations::tool_calls))
        .routes(routes!(conversations::cancel))
        .routes(routes!(conversations::delete))
        .routes(routes!(prompt::assist))
        .routes(routes!(workflows::list))
        .routes(routes!(workflows::refresh))
        .routes(routes!(workflows::import))
        .routes(routes!(workflows::import_confirm))
        .routes(routes!(packs::list))
        .routes(routes!(packs::validate))
        .routes(routes!(workflows::details))
        .routes(routes!(workflows::validate))
        .routes(routes!(workflows::thumbnail))
        .routes(routes!(batches::list, batches::submit))
        .routes(routes!(batches::preview))
        .routes(routes!(batches::get_batch))
        .routes(routes!(batches::resume))
        .routes(routes!(runs::list, runs::submit))
        .routes(routes!(runs::reconcile_all))
        .routes(routes!(runs::get_run))
        .routes(routes!(runs::rerun))
        .routes(routes!(runs::events))
        .routes(routes!(runs::cancel))
        .routes(routes!(runs::reconcile))
        .routes(routes!(outputs::manifest))
        .routes(routes!(outputs::resolved_workflow))
        .routes(routes!(outputs::list))
        .routes(routes!(outputs::metadata))
        .routes(routes!(outputs::review))
        .routes(routes!(outputs::annotations))
        .routes(routes!(outputs::original))
        .routes(routes!(outputs::thumbnail))
        .routes(routes!(uploads::upload))
        .routes(routes!(uploads::metadata))
        .routes(routes!(models::list_llm))
        .routes(routes!(models::start_pull, models::list_pulls))
        .routes(routes!(models::get_pull, models::cancel_pull))
        .routes(routes!(models::pull_events))
        .routes(routes!(models::checkpoints))
        .routes(routes!(settings::read, settings::save))
        .routes(routes!(settings::probe))
        .routes(routes!(setup::status))
        .routes(routes!(setup::detect))
}

pub(crate) fn router_and_openapi() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let (api_router, api_doc) = api().split_for_parts();
    let api_router = api_router
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
        });
    // Reattach the matching document before nesting, so both paths and router
    // receive the same prefix while the fallback remains on the inner router.
    let api = OpenApiRouter::with_openapi(api_doc).merge(api_router.into());
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1", api)
        .split_for_parts()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[utoipa::path(get, path = "/added-for-test", responses((status = 200, description = "OK")))]
    async fn added_for_test() {}

    #[test]
    fn adding_a_route_also_adds_its_contract() {
        let router = api().routes(routes!(added_for_test));
        assert!(router.get_openapi().paths.paths["/added-for-test"]
            .get
            .is_some());
    }
}
