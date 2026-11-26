use std::{
    io::{self, Write},
    process::ExitCode,
};

use genaimage_api::{client, ModelPull, ModelPullState, ProblemDetails, RunResponse};
use serde::Serialize;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Success = 0,
    LocalFailure = 1,
    // Clap owns exit code 2 for argument errors.
    ServerNotRunning = 3,
    ServerProblem = 4,
    ClientFailure = 5,
    NegativeResult = 6,
    UnsuccessfulRun = 7,
    UnknownSubmission = 8,
    WaitTimeout = 9,
    ModelPullFailed = 10,
}

impl From<Status> for ExitCode {
    fn from(status: Status) -> Self {
        Self::from(status as u8)
    }
}

pub(crate) enum Error {
    Api(client::Error),
    Input(String),
    Browser(String),
    WaitTimeout(String),
    File(String),
    Submit(client::Error),
    UnconfirmedRun(Box<RunResponse>),
    Observe { run_id: String, error: Box<Error> },
    Output(serde_json::Error),
    Io(io::Error),
    PullFailed(Box<ModelPull>),
    PullUnknown(String),
}

impl From<client::Error> for Error {
    fn from(error: client::Error) -> Self {
        Self::Api(error)
    }
}

#[derive(Serialize)]
struct ErrorReport<'a> {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<&'a ProblemDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<&'a RunResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
}

impl ErrorReport<'_> {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            problem: None,
            run: None,
            run_id: None,
        }
    }
}

impl Error {
    fn describe(&self) -> (Status, ErrorReport<'_>) {
        match self {
            Self::Api(client::Error::ServerNotRunning { base_url, .. })
            | Self::Submit(client::Error::ServerNotRunning { base_url, .. }) => (
                Status::ServerNotRunning,
                ErrorReport::new("server_not_running", format!(
                    "Cannot connect to the GenAImage server at {base_url}. Start it with `genaimage serve`. If it uses a different address, set --server or GENAIMAGE_SERVER."
                )),
            ),
            Self::Api(client::Error::Problem(problem)) => (
                Status::ServerProblem,
                ErrorReport {
                    kind: "server_problem",
                    message: format!("{}: {} {}", problem.title, problem.detail, problem.recovery),
                    problem: Some(problem),
                    run: None,
                    run_id: None,
                },
            ),
            Self::Submit(client::Error::UnknownSubmission(run)) | Self::UnconfirmedRun(run) => (
                Status::UnknownSubmission,
                ErrorReport {
                    run: Some(run),
                    ..ErrorReport::new("unknown_submission", format!(
                        "Submission of run '{}' is unconfirmed. Use `genaimage jobs reconcile {}` before considering another submission.", run.id, run.id
                    ))
                },
            ),
            Self::Submit(client::Error::FailedSubmission(run)) => (
                Status::UnsuccessfulRun,
                ErrorReport {
                    run: Some(run),
                    ..ErrorReport::new("failed_submission", format!(
                        "Submission of run '{}' failed: {}. Inspect it with `genaimage jobs show {}` or use `genaimage jobs reconcile {}` to check backend state.",
                        run.id, run.error.as_deref().unwrap_or("no failure detail supplied"), run.id, run.id
                    ))
                },
            ),
            Self::Submit(error) if uncertain_response(error) => (
                Status::UnknownSubmission,
                ErrorReport {
                    problem: match error {
                        client::Error::Problem(problem) => Some(problem),
                        _ => None,
                    },
                    ..ErrorReport::new("unknown_submission", format!(
                        "No conclusive submission response: {error} Check `genaimage jobs list` and use `genaimage jobs reconcile` before considering another submission."
                    ))
                },
            ),
            Self::Submit(client::Error::Problem(problem)) => (
                Status::ServerProblem,
                ErrorReport {
                    problem: Some(problem),
                    ..ErrorReport::new("server_problem", format!(
                        "{}: {} {} If a run was recorded, check `genaimage jobs list` and use `genaimage jobs reconcile`.", problem.title, problem.detail, problem.recovery
                    ))
                },
            ),
            Self::Api(error) | Self::Submit(error) => (
                Status::ClientFailure,
                ErrorReport::new("client_error", error.to_string()),
            ),
            Self::Observe { run_id, error } => {
                let (status, mut report) = error.describe();
                report.run_id = Some(run_id);
                report.message.push_str(&format!(" Could not confirm the state of run '{run_id}'. Use `genaimage jobs show {run_id}` or `genaimage jobs reconcile {run_id}`."));
                (status, report)
            }
            Self::PullFailed(pull) => (
                Status::ModelPullFailed,
                ErrorReport::new("model_pull_failed", format!(
                    "Pull of '{}' {}: {}",
                    pull.name,
                    if pull.state == ModelPullState::Cancelled { "was cancelled" } else { "failed" },
                    pull.error.as_deref().unwrap_or("no detail supplied"),
                )),
            ),
            Self::PullUnknown(id) => (
                Status::ClientFailure,
                ErrorReport::new("model_pull_unknown", format!(
                    "The server no longer knows pull '{id}', probably because it restarted, so its outcome is unknown. Run `genaimage models list` to see whether the model is installed."
                )),
            ),
            Self::WaitTimeout(message) => (Status::WaitTimeout, ErrorReport::new("wait_timeout", message)),
            Self::File(message) => (Status::LocalFailure, ErrorReport::new("file_error", message)),
            Self::Input(message) => (Status::LocalFailure, ErrorReport::new("input_error", message)),
            Self::Browser(message) => (Status::LocalFailure, ErrorReport::new("browser_error", message)),
            Self::Output(error) => (
                Status::LocalFailure,
                ErrorReport::new("output_error", error.to_string()),
            ),
            Self::Io(error) => (
                Status::LocalFailure,
                ErrorReport::new("output_error", error.to_string()),
            ),
        }
    }
}

fn uncertain_response(error: &client::Error) -> bool {
    match error {
        client::Error::Http(_) | client::Error::HttpStatus { .. } => true,
        client::Error::Problem(problem) => problem.status >= 500 || problem.status == 408,
        _ => false,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Output {
    json: bool,
}

impl Output {
    pub(crate) fn new(json: bool) -> Self {
        Self { json }
    }

    pub(crate) fn write<T: Serialize>(
        self,
        value: &T,
        human: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> Result<(), Error> {
        let mut stdout = io::stdout().lock();
        if self.json {
            let rendered = serde_json::to_string(value).map_err(Error::Output)?;
            writeln!(stdout, "{rendered}").map_err(Error::Io)
        } else {
            human(&mut stdout).map_err(Error::Io)
        }
    }

    pub(crate) fn is_json(self) -> bool {
        self.json
    }

    pub(crate) fn progress(self, message: impl std::fmt::Display) {
        if !self.json {
            let _ = writeln!(io::stderr().lock(), "{message}");
        }
    }

    pub(crate) fn report_local(self, error: &impl std::fmt::Display) -> ExitCode {
        if self.json {
            eprintln!(
                "{}",
                serde_json::json!({ "status": "error", "error": error.to_string() })
            );
        } else {
            eprintln!("Error: {error}");
        }
        Status::LocalFailure.into()
    }

    pub(crate) fn report(self, error: &Error) -> ExitCode {
        let (status, report) = error.describe();
        let mut stderr = io::stderr().lock();
        if self.json {
            if let Ok(rendered) = serde_json::to_string(&report) {
                let _ = writeln!(stderr, "{rendered}");
            }
        } else {
            let _ = writeln!(stderr, "Error: {}", report.message);
        }
        status.into()
    }
}
