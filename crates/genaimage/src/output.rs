use std::{
    io::{self, Write},
    process::ExitCode,
};

use genaimage_api::{client, ProblemDetails};
use serde::Serialize;

pub(crate) enum Status {
    Success = 0,
    LocalFailure = 1,
    // Clap owns exit code 2 for argument errors.
    ServerNotRunning = 3,
    ServerProblem = 4,
    ClientFailure = 5,
    NegativeResult = 6,
}

impl From<Status> for ExitCode {
    fn from(status: Status) -> Self {
        Self::from(status as u8)
    }
}

pub(crate) enum Error {
    Api(client::Error),
    Input(String),
    Output(serde_json::Error),
    Io(io::Error),
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
}

impl ErrorReport<'_> {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            problem: None,
        }
    }
}

impl Error {
    fn describe(&self) -> (Status, ErrorReport<'_>) {
        match self {
            Self::Api(client::Error::ServerNotRunning { base_url, .. }) => (
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
                },
            ),
            Self::Api(error) => (
                Status::ClientFailure,
                ErrorReport::new("client_error", error.to_string()),
            ),
            Self::Input(message) => (Status::LocalFailure, ErrorReport::new("input_error", message)),
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
