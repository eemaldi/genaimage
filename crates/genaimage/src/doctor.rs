use std::{io, time::Duration};

use genaimage_api::{
    client::{self, Client},
    DoctorCheck, DoctorReport, DoctorStatus, DOCTOR_CHECK_IDS,
};
use tokio::{net::TcpStream, time::timeout};

use crate::output::{Error, Output, Status};

/// The server's own probes take up to a few seconds; a longer silence means it is stuck.
const REPORT_DEADLINE: Duration = Duration::from_secs(20);
const PORT_PROBE_DEADLINE: Duration = Duration::from_secs(1);

enum Failure {
    NotRunning,
    NotGenAImage,
    Problem(Box<genaimage_api::ProblemDetails>),
}

pub(crate) async fn run(client: &Client, output: Output) -> Result<Status, Error> {
    let (report, failure) = match timeout(REPORT_DEADLINE, client.doctor()).await {
        Ok(Ok(report)) => (report, None),
        Ok(Err(client::Error::ServerNotRunning { .. })) => offline(client, Failure::NotRunning),
        Ok(Err(client::Error::Problem(problem))) => offline(client, Failure::Problem(problem)),
        Ok(Err(
            client::Error::Http(_)
            | client::Error::HttpStatus { .. }
            | client::Error::Connect { .. },
        ))
        | Err(_) => {
            let failure = if listening(client).await {
                Failure::NotGenAImage
            } else {
                Failure::NotRunning
            };
            offline(client, failure)
        }
        Ok(Err(error)) => return Err(error.into()),
    };
    output.write(&report, |out| render(out, &report))?;
    Ok(match failure {
        Some(Failure::NotRunning) => Status::ServerNotRunning,
        Some(Failure::Problem(_)) => Status::ServerProblem,
        _ if report
            .checks
            .iter()
            .any(|check| check.status == DoctorStatus::Error) =>
        {
            Status::NegativeResult
        }
        _ => Status::Success,
    })
}

async fn listening(client: &Client) -> bool {
    let url = client.base_url();
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return false;
    };
    matches!(
        timeout(
            PORT_PROBE_DEADLINE,
            TcpStream::connect(format!("{host}:{port}"))
        )
        .await,
        Ok(Ok(_))
    )
}

fn check(
    id: &str,
    status: DoctorStatus,
    summary: impl Into<String>,
    fix: Option<String>,
) -> DoctorCheck {
    DoctorCheck {
        id: id.into(),
        status,
        summary: summary.into(),
        items: Vec::new(),
        fix,
    }
}

/// The full, stable set of checks, with every check the server would have run skipped.
fn offline(client: &Client, failure: Failure) -> (DoctorReport, Option<Failure>) {
    let origin = client.base_url().as_str().trim_end_matches('/').to_owned();
    let port = client
        .base_url()
        .port_or_known_default()
        .unwrap_or_default();
    let start = "Start it with `genaimage serve`. If it uses a different address, pass --server or set GENAIMAGE_SERVER.";
    let (server, ports) = match &failure {
        Failure::NotRunning => (
            check(
                "server",
                DoctorStatus::Error,
                format!("No GenAImage server answered at {origin}."),
                Some(start.into()),
            ),
            check("ports", DoctorStatus::Ok, format!("Port {port} is free: nothing is listening there."), None),
        ),
        Failure::NotGenAImage => (
            check(
                "server",
                DoctorStatus::Error,
                format!("Something is listening at {origin}, but it is not a GenAImage server."),
                Some("Stop that program or start GenAImage on another port: set `server.port`, run `genaimage serve`, and pass --server or set GENAIMAGE_SERVER to the new address.".into()),
            ),
            check(
                "ports",
                DoctorStatus::Error,
                format!("Port {port} is occupied by a different process."),
                Some(format!("Find the process that owns port {port} (for example `lsof -i :{port}` or `netstat -ano`), stop it, or choose another `server.port` for `genaimage serve`.")),
            ),
        ),
        Failure::Problem(problem) => (
            check(
                "server",
                DoctorStatus::Error,
                format!("The server at {origin} refused the diagnostics request: {}: {}", problem.title, problem.detail),
                Some(format!("{} If this server predates `genaimage doctor`, restart it with the current `genaimage serve`.", problem.recovery)),
            ),
            check("ports", DoctorStatus::Ok, format!("Port {port} is served by GenAImage."), None),
        ),
    };
    let checks = DOCTOR_CHECK_IDS
        .iter()
        .map(|id| match *id {
            "server" => server.clone(),
            "ports" => ports.clone(),
            id => check(
                id,
                DoctorStatus::Skipped,
                "Not run: the GenAImage server is not available.",
                Some("Resolve the `server` check, then run `genaimage doctor` again.".into()),
            ),
        })
        .collect();
    (DoctorReport { checks }, Some(failure))
}

fn render(out: &mut dyn io::Write, report: &DoctorReport) -> io::Result<()> {
    for check in &report.checks {
        let label = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warning => "warning",
            DoctorStatus::Error => "ERROR",
            DoctorStatus::Skipped => "skipped",
        };
        writeln!(out, "[{label:<7}] {:<20} {}", check.id, check.summary)?;
        for item in &check.items {
            writeln!(out, "{:>31}- {item}", "")?;
        }
        if let (Some(fix), true) = (&check.fix, check.status != DoctorStatus::Ok) {
            writeln!(out, "{:>31}Fix: {fix}", "")?;
        }
    }
    let count = |status| {
        report
            .checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    };
    writeln!(
        out,
        "Errors: {}, warnings: {}, skipped: {}.",
        count(DoctorStatus::Error),
        count(DoctorStatus::Warning),
        count(DoctorStatus::Skipped)
    )
}
