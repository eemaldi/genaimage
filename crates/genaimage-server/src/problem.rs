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
                preview: None,
                preview_token: None,
                suggested_id: None,
                expected_graph_sha256: None,
                id: None,
                manifest_path: None,
                invalid_items: Vec::new(),
                invalid_item_count: None,
                current_sha256: None,
            }),
        }
    }

    pub fn assistant_unavailable(error: genaimage_core::AssistantUnavailable) -> Self {
        let recovery = match &error {
            genaimage_core::AssistantUnavailable::NotConfigured { .. } => {
                "Set llm.base_url and the requested model role in Settings, then restart."
            }
            genaimage_core::AssistantUnavailable::Unreachable { .. } => {
                "Check that the local LLM service is running, then test the connection in Settings."
            }
            genaimage_core::AssistantUnavailable::ModelNotFound { .. } => {
                "Choose a model returned by the LLM connection test."
            }
            genaimage_core::AssistantUnavailable::CapabilityUnsupported { .. } => {
                "Choose a model with the required capability, or add an explicit reviewed capability override."
            }
        };
        Self::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string(), recovery)
            .with_type("urn:genaimage:problem:assistant-unavailable")
    }

    pub fn invalid_llm_request(error: genaimage_core::HarnessError) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            "Use the model pinned by the selected LLM role and a model with the required tools or vision capability.",
        )
    }

    pub(crate) fn workflow_changed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            genaimage_core::HarnessError::WorkflowChanged.to_string(),
            "Reload the workflow and review it again before submitting.",
        )
        .with_type("urn:genaimage:problem:workflow-changed")
    }

    /// Several manifests claim one workflow ID; the detail names every claimant.
    pub(crate) fn workflow_ambiguous(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            detail,
            "Remove or rename the duplicate workflow so exactly one manifest declares the ID, then refresh the workflow catalogue.",
        )
        .with_type("urn:genaimage:problem:workflow-ambiguous")
    }

    /// An import was written, but another manifest declares its ID. The
    /// written workflow stays on disk; a retry must not create another copy.
    pub(crate) fn workflow_written_but_ambiguous(
        id: &str,
        manifest_path: &str,
        reason: &str,
    ) -> Self {
        let mut problem = Self::new(
            StatusCode::CONFLICT,
            format!(
                "Workflow '{id}' was written at '{manifest_path}' and stays unavailable until the duplicate is removed or renamed: {reason}"
            ),
            "Do not confirm the import again. Remove or rename the duplicate workflow so exactly one manifest declares the ID, then refresh the workflow catalogue.",
        )
        .with_type("urn:genaimage:problem:workflow-ambiguous");
        problem.details.id = Some(id.to_owned());
        problem.details.manifest_path = Some(manifest_path.to_owned());
        problem
    }

    pub(crate) fn invalid_path(error: PathRejection, recovery: &str) -> Self {
        Self::new(error.status(), error.body_text(), recovery)
    }

    pub(crate) fn with_type(mut self, problem_type: &str) -> Self {
        self.details.problem_type = problem_type.into();
        self
    }

    pub(crate) fn with_suggested_id(mut self, suggested_id: String) -> Self {
        self.details.suggested_id = Some(suggested_id);
        self
    }

    pub(crate) fn with_expected_graph_sha256(mut self, sha256: String) -> Self {
        self.details.expected_graph_sha256 = Some(sha256);
        self
    }

    pub(crate) fn with_invalid_items(
        mut self,
        total: usize,
        items: Vec<genaimage_api::BatchItemProblem>,
    ) -> Self {
        self.details.invalid_item_count = Some(total);
        self.details.invalid_items = items;
        self
    }

    pub(crate) fn with_current_sha256(mut self, sha256: String) -> Self {
        self.details.current_sha256 = Some(sha256);
        self
    }

    pub(crate) fn with_errors(mut self, errors: Vec<SettingError>) -> Self {
        self.details.errors = errors;
        self
    }

    pub(crate) fn with_preview(
        mut self,
        preview: Option<serde_json::Value>,
        preview_token: Option<String>,
    ) -> Self {
        self.details.preview = preview;
        self.details.preview_token = preview_token;
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
