use axum::{
    extract::rejection::PathRejection,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use genaimage_api::{ProblemDetails, SettingError};

/// Converts a problem into an `application/problem+json` HTTP response.
pub struct Problem {
    status: StatusCode,
    details: Box<ProblemDetails>,
}

impl Problem {
    /// Constructs a generic problem with semantics given by its HTTP status.
    pub fn new(status: StatusCode, detail: impl Into<String>, recovery: impl Into<String>) -> Self {
        Self {
            status,
            details: Box::new(ProblemDetails {
                problem_type: "about:blank".into(),
                title: status.canonical_reason().unwrap_or("Unknown status").into(),
                status: status.as_u16(),
                detail: detail.into(),
                recovery: recovery.into(),
                errors: Vec::new(),
            }),
        }
    }

    pub(crate) fn workflow_changed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            genaimage_core::HarnessError::WorkflowChanged.to_string(),
            "Reload the workflow and review it again before submitting.",
        )
        .with_type("urn:genaimage:problem:workflow-changed")
    }

    pub(crate) fn invalid_path(error: PathRejection, recovery: &str) -> Self {
        Self::new(error.status(), error.body_text(), recovery)
    }

    pub(crate) fn with_type(mut self, problem_type: &str) -> Self {
        self.details.problem_type = problem_type.into();
        self
    }

    pub(crate) fn with_errors(mut self, errors: Vec<SettingError>) -> Self {
        self.details.errors = errors;
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(self.details),
        )
            .into_response()
    }
}
