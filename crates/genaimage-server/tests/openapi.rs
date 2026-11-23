mod common;

use common::ApiHarness;
use reqwest::{header, Method, StatusCode};

#[test]
fn checked_in_openapi_matches_generated_document() {
    let generated = serde_json::to_value(genaimage_server::openapi()).unwrap();
    let expected = format!("{}\n", serde_json::to_string_pretty(&generated).unwrap());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/openapi.json");
    let actual = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        actual, expected,
        "OpenAPI drift: run cargo run -p genaimage-server --example openapi"
    );
}

#[tokio::test]
async fn served_contract_matches_generated_contract_and_is_hardened() {
    let api = ApiHarness::start().await;
    let response = api
        .request(Method::GET, "/api/v1/openapi.json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        value,
        serde_json::to_value(genaimage_server::openapi()).unwrap()
    );
    let response = api
        .request(Method::GET, "/api/v1/openapi.json")
        .header(header::HOST, "evil.example:80")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::MISDIRECTED_REQUEST).await;
}

#[test]
fn thumbnail_size_parameter_matches_supported_sizes() {
    let spec = serde_json::to_value(genaimage_server::openapi()).unwrap();
    let params = spec["paths"]["/api/v1/outputs/{id}/thumbnail"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let size = params.iter().find(|param| param["name"] == "size").unwrap();
    assert_eq!(size["required"], true);
    let reference = size["schema"]["$ref"].as_str().unwrap();
    let schema = spec.pointer(reference.strip_prefix('#').unwrap()).unwrap();
    let documented: Vec<u32> = schema["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| u32::try_from(value.as_u64().unwrap()).unwrap())
        .collect();
    let supported: Vec<u32> = (0..=2048)
        .filter(|&size| genaimage_core::media::ThumbnailSize::new(size).is_ok())
        .collect();
    assert_eq!(documented, supported);
}

#[tokio::test]
async fn documented_operations_reach_the_real_router() {
    let api = ApiHarness::start().await;
    let spec = serde_json::to_value(genaimage_server::openapi()).unwrap();
    for (path, operations) in spec["paths"].as_object().unwrap() {
        for (method, _) in operations.as_object().unwrap() {
            let method = Method::from_bytes(method.to_uppercase().as_bytes()).unwrap();
            let mut url = path.replace("{id}", "not-found");
            if url.ends_with("/outputs/not-found/thumbnail") {
                url.push_str("?size=128");
            }
            let response = api.request(method.clone(), &url).send().await.unwrap();
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {url}"
            );
            if response.status() == StatusCode::NOT_FOUND && method != Method::HEAD {
                let body = response.text().await.unwrap();
                assert!(
                    !body.contains("No API endpoint matches this path."),
                    "{method} {url}"
                );
            }
        }
    }
}
