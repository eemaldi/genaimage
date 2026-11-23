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
