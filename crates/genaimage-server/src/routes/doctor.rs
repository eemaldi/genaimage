use axum::{extract::State, Json};
use genaimage_api::{DoctorCheck, DoctorReport, DoctorStatus};
use genaimage_core::doctor::{self, DoctorOptions};

use crate::AppState;

#[utoipa::path(
    get, path = "/doctor",
    operation_id = "doctor_report",
    tag = "doctor",
    description = "Run diagnostics over the configuration, ComfyUI, workflows, the optional LLM, GPU, disk space and ports. Every check that is not ok has a suggested fix.",
    responses(
        (status = 200, body = genaimage_api::DoctorReport),
        (status = 400, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 408, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 413, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 421, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 500, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json"),
        (status = 503, description = "Problem details", body = genaimage_api::ProblemDetails, content_type = "application/problem+json")
    )
)]
pub(super) async fn doctor(State(state): State<AppState>) -> Json<DoctorReport> {
    let restart_required = state.settings.get().1.restart_required;
    let report = doctor::run(
        &state.harness(),
        state.llm_roles(),
        &restart_required,
        &DoctorOptions::default(),
    )
    .await;
    Json(DoctorReport {
        checks: report
            .checks
            .into_iter()
            .map(|check| DoctorCheck {
                id: check.id,
                status: match check.status {
                    doctor::DoctorStatus::Ok => DoctorStatus::Ok,
                    doctor::DoctorStatus::Warning => DoctorStatus::Warning,
                    doctor::DoctorStatus::Error => DoctorStatus::Error,
                    doctor::DoctorStatus::Skipped => DoctorStatus::Skipped,
                },
                summary: check.summary,
                items: check.items,
                fix: check.fix,
            })
            .collect(),
    })
}
