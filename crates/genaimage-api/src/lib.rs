//! Shared wire types for the local API.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    #[serde(rename = "self")]
    pub server: HealthStatus,
    pub image_backend: HealthStatus,
    pub llm: HealthStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Unavailable,
    NotConfigured,
}

/// RFC 9457 problem details with a GenAImage recovery hint.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub recovery: String,
}
