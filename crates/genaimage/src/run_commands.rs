use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::Path,
    time::Duration,
};

use futures_util::StreamExt;
use genaimage_api::{
    client::{Client, EventsQuery, StreamEvent},
    BackendEvent, CharacterLink, Event, OutputListQuery, OutputResponse, ReconcileOutcome,
    ReconcileRunResponse, RunEventResponse, RunEventsQuery, RunListQuery, RunResponse, RunStatus,
    SubmitRunRequest,
};
use serde::Serialize;
use serde_json::Value;
use tokio::{
    fs,
    io::AsyncReadExt,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::{
    output::{Error, Output, Status},
    parse_parameters, JobsCommand, RunCommand,
};

const STATUS_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const DEFAULT_WAIT_SECONDS: u64 = 600;
const STATUS_REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const STREAM_CONNECT_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) async fn run(
    client: &Client,
    output: Output,
    command: &RunCommand,
) -> Result<Status, Error> {
    let mut parameters = parse_parameters(&command.parameters).map_err(Error::Input)?;
    if !command.images.is_empty() {
        upload_images(client, &command.workflow, &command.images, &mut parameters).await?;
    }
    let request = SubmitRunRequest {
        workflow_revision: None,
        workflow: command.workflow.clone(),
        preset: command.preset.clone(),
        checkpoint_profile: command.checkpoint_profile.clone(),
        checkpoint_profile_revision: None,
        parameters,
        version: command.version.clone(),
        character: command
            .character
            .as_deref()
            .map(parse_character_link)
            .transpose()?,
    };
    let run = client
        .submit_run(&Uuid::new_v4().to_string(), &request)
        .await
        .map_err(Error::Submit)?;
    if command.wait {
        let seconds = command.timeout.unwrap_or(DEFAULT_WAIT_SECONDS);
        let id = run.id.clone();
        return finish(output, wait(client, output, &id, Some(run), seconds).await?);
    }
    write_run(output, &run)?;
    Ok(if run.status == RunStatus::Cancelled {
        Status::UnsuccessfulRun
    } else {
        Status::Success
    })
}

fn parse_character_link(link: &str) -> Result<CharacterLink, Error> {
    let invalid = || {
        Error::Input(format!(
            "'{link}' is not a character link; use ID:VERSION such as char-…:2."
        ))
    };
    let (id, version) = link.rsplit_once(':').ok_or_else(invalid)?;
    let canonical = !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
        && (version == "0" || !version.starts_with('0'));
    match version.parse() {
        Ok(spec_version) if canonical && !id.is_empty() => Ok(CharacterLink {
            id: id.to_owned(),
            spec_version,
        }),
        _ => Err(invalid()),
    }
}

async fn upload_images(
    client: &Client,
    workflow: &str,
    images: &[String],
    parameters: &mut BTreeMap<String, Value>,
) -> Result<(), Error> {
    let mut paths = BTreeMap::new();
    for image in images {
        let (name, path) = image
            .split_once('=')
            .filter(|(name, path)| !name.trim().is_empty() && !path.is_empty())
            .ok_or_else(|| Error::Input("Image bindings must use NAME=PATH form.".into()))?;
        if parameters.contains_key(name) || paths.insert(name, Path::new(path)).is_some() {
            return Err(Error::Input(format!(
                "Image parameter '{name}' was specified more than once."
            )));
        }
    }
    let details = client.workflow(workflow).await?;
    for name in paths.keys() {
        if !details
            .parameters
            .get(*name)
            .is_some_and(|parameter| parameter.kind == "image_upload")
        {
            return Err(Error::Input(format!(
                "'{name}' is not an image_upload parameter of workflow '{workflow}'."
            )));
        }
    }
    let settings = client.settings().await?;
    let limit = settings
        .settings
        .get("server.max_upload_bytes")
        .and_then(|setting| {
            setting
                .active_value
                .as_ref()
                .unwrap_or(&setting.value)
                .as_u64()
        })
        .ok_or_else(|| {
            Error::Input("The server did not report a valid image upload limit.".into())
        })?;
    for (name, path) in paths {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::Input(format!(
                    "Image path '{}' needs a UTF-8 filename.",
                    path.display()
                ))
            })?;
        let bytes = read_image(path, limit).await.map_err(|error| {
            Error::Input(format!("Cannot read image '{}': {error}", path.display()))
        })?;
        let upload = client.upload(filename, bytes).await?;
        parameters.insert(name.to_owned(), Value::String(upload.upload_id));
    }
    Ok(())
}

async fn read_image(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let check = |metadata: std::fs::Metadata| {
        if !metadata.is_file() {
            return Err(io::Error::other("expected a regular file"));
        }
        if metadata.len() > limit {
            return Err(io::Error::other(format!(
                "exceeds the server's {limit}-byte upload limit"
            )));
        }
        Ok(())
    };
    check(fs::metadata(path).await?)?;
    let file = fs::File::open(path).await?;
    check(file.metadata().await?)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(format!(
            "exceeds the server's {limit}-byte upload limit"
        )));
    }
    Ok(bytes)
}

#[derive(Serialize)]
struct JobDetails {
    run: RunResponse,
    events: Vec<RunEventResponse>,
    outputs: Vec<OutputResponse>,
    next_output_cursor: Option<String>,
}

pub(crate) async fn jobs(
    client: &Client,
    output: Output,
    command: &JobsCommand,
) -> Result<Status, Error> {
    match command {
        JobsCommand::List { limit, cursor } => {
            let page = client
                .runs(&RunListQuery {
                    limit: Some(*limit),
                    cursor: cursor.clone(),
                    ..Default::default()
                })
                .await?;
            output.write(&page, |out| {
                write_runs(out, &page.runs)?;
                if let Some(cursor) = &page.next_cursor {
                    writeln!(out, "Next page: --cursor {cursor}")?;
                }
                Ok(())
            })?;
        }
        JobsCommand::Show {
            run_id,
            events,
            output_cursor,
        } => {
            let run = client.run(run_id).await?;
            let event_query = RunEventsQuery {
                limit: Some(*events),
            };
            let output_query = OutputListQuery {
                run: Some(run_id.clone()),
                cursor: output_cursor.clone(),
                ..Default::default()
            };
            let (events, outputs) = tokio::try_join!(
                client.run_events(run_id, &event_query),
                client.outputs(&output_query),
            )?;
            let details = JobDetails {
                run,
                events: events.events,
                outputs: outputs.outputs,
                next_output_cursor: outputs.next_cursor,
            };
            output.write(&details, |out| {
                write_summary(out, &details.run)?;
                writeln!(out, "Events:")?;
                for event in &details.events {
                    writeln!(
                        out,
                        "  {}  {}  {}",
                        event.created_at,
                        event.event_type,
                        event.message.as_deref().unwrap_or("")
                    )?;
                }
                writeln!(out, "Outputs:")?;
                for item in &details.outputs {
                    writeln!(
                        out,
                        "  {}  {} ({})",
                        item.id, item.source_filename, item.content_type
                    )?;
                }
                writeln!(out, "Retrieve artefacts with `genaimage jobs download {run_id} --dir DIR` or `genaimage jobs manifest {run_id}`.")?;
                if let Some(cursor) = &details.next_output_cursor {
                    writeln!(out, "Next output page: --output-cursor {cursor}")?;
                }
                Ok(())
            })?;
        }
        JobsCommand::Wait { run_id, timeout } => {
            return finish(output, wait(client, output, run_id, None, *timeout).await?);
        }
        JobsCommand::Download { run_id, dir } => {
            crate::artefacts::download(client, output, run_id, dir).await?
        }
        JobsCommand::Manifest { run_id, out } => {
            crate::artefacts::manifest(client, output, run_id, out.as_deref()).await?
        }
        JobsCommand::Cancel { run_id } => write_run(output, &client.cancel_run(run_id).await?)?,
        JobsCommand::Reconcile {
            run_id: Some(run_id),
        } => {
            let result = client.reconcile_run_with_outcome(run_id).await?;
            output.write(&result, |out| write_reconciled_run(out, &result))?;
        }
        JobsCommand::Reconcile { run_id: None } => {
            let result = client.reconcile_runs_with_outcomes().await?;
            output.write(&result, |out| {
                if result.runs.is_empty() {
                    writeln!(out, "No runs found.")?;
                }
                for run in &result.runs {
                    write_reconciled_run(out, run)?;
                }
                Ok(())
            })?;
        }
    }
    Ok(Status::Success)
}

async fn wait(
    client: &Client,
    output: Output,
    id: &str,
    initial: Option<RunResponse>,
    seconds: u64,
) -> Result<RunResponse, Error> {
    let result = timeout(Duration::from_secs(seconds), async {
        let initial = match initial {
            Some(run) => run,
            None => status(client, id).await?,
        };
        if done(initial.status) {
            return Ok(initial);
        }
        observe(client, output, id).await
    })
    .await
    .unwrap_or_else(|_| {
        Err(Error::WaitTimeout(format!(
            "Wait exceeded {seconds} seconds."
        )))
    });
    result.map_err(|error| Error::Observe {
        run_id: id.to_owned(),
        error: Box::new(error),
    })
}

async fn status(client: &Client, id: &str) -> Result<RunResponse, Error> {
    timeout(STATUS_REQUEST_DEADLINE, client.run(id))
        .await
        .map_err(|_| Error::WaitTimeout("Run status request timed out after 5 seconds.".into()))?
        .map_err(Error::from)
}

async fn observe(client: &Client, output: Output, id: &str) -> Result<RunResponse, Error> {
    let query = EventsQuery {
        run_id: Some(id),
        ..Default::default()
    };
    let mut stream = timeout(STREAM_CONNECT_DEADLINE, client.events(&query))
        .await
        .ok()
        .and_then(Result::ok);
    // Subscribe before reading the snapshot so completion cannot fall between them.
    if stream.is_none() {
        output.progress(format_args!(
            "{id}: live updates unavailable; checking run status instead."
        ));
    }
    loop {
        let current = status(client, id).await?;
        if done(current.status) {
            return Ok(current);
        }
        let Some(events) = stream.as_mut() else {
            sleep(STATUS_INTERVAL).await;
            continue;
        };
        let refresh = sleep(STATUS_INTERVAL);
        tokio::pin!(refresh);
        loop {
            tokio::select! {
                () = &mut refresh => break,
                event = events.next() => match event {
                    Some(Ok(StreamEvent::Event(envelope))) => match envelope.event {
                        Event::RunProgress { run, backend_event } if run.id == id => {
                            print_progress(output, id, backend_event.as_ref());
                            if done(run.status) { break; }
                        }
                        Event::RunStarted { run } | Event::RunQueued { run } if run.id == id => {
                            output.progress(format_args!("{id}: {}", status_label(run.status)));
                        }
                        Event::RunCompleted { run } | Event::RunFailed { run } |
                        Event::RunCancelled { run } | Event::RunUnknownSubmission { run } if run.id == id => break,
                        _ => {}
                    },
                    _ => {
                        output.progress(format_args!("{id}: live updates interrupted; checking run status instead."));
                        stream = None;
                        break;
                    }
                }
            }
        }
    }
}

fn print_progress(output: Output, id: &str, event: Option<&BackendEvent>) {
    if let Some(event) = event {
        if let Some((value, maximum)) = event.value.zip(event.maximum) {
            output.progress(format_args!("{id}: {} {value}/{maximum}", event.event_type));
        } else {
            output.progress(format_args!("{id}: {}", event.event_type));
        }
    }
}

fn done(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed
            | RunStatus::Failed
            | RunStatus::Cancelled
            | RunStatus::UnknownSubmission
    )
}

fn finish(output: Output, run: RunResponse) -> Result<Status, Error> {
    if run.status == RunStatus::UnknownSubmission {
        return Err(Error::UnconfirmedRun(Box::new(run)));
    }
    write_run(output, &run)?;
    Ok(if run.status == RunStatus::Completed {
        Status::Success
    } else {
        Status::UnsuccessfulRun
    })
}

fn write_run(output: Output, run: &RunResponse) -> Result<(), Error> {
    output.write(run, |out| write_summary(out, run))
}

fn write_runs(out: &mut dyn Write, runs: &[RunResponse]) -> io::Result<()> {
    if runs.is_empty() {
        writeln!(out, "No runs found.")?;
    }
    for run in runs {
        write_summary(out, run)?;
    }
    Ok(())
}

fn write_reconciled_run(out: &mut dyn Write, result: &ReconcileRunResponse) -> io::Result<()> {
    write_summary(out, &result.run)?;
    let outcome = match result.outcome {
        ReconcileOutcome::Terminal => "Run is terminal",
        ReconcileOutcome::Active => "Prompt found in the queue",
        ReconcileOutcome::InProgressElsewhere => "Submission is still in progress elsewhere",
        ReconcileOutcome::NotFound => "Prompt not found in queue or history",
        ReconcileOutcome::StillAmbiguous => "Run state is still ambiguous",
    };
    writeln!(out, "  Reconciliation: {outcome}")?;
    if let Some(instant) = result.settles_at {
        writeln!(
            out,
            "  Reconcile again at or after {}",
            instant.to_rfc3339()
        )?;
    }
    Ok(())
}

fn write_summary(out: &mut dyn Write, run: &RunResponse) -> io::Result<()> {
    writeln!(
        out,
        "{}  {}  {}",
        run.id,
        run.workflow_id,
        status_label(run.status)
    )?;
    if let Some(error) = &run.error {
        writeln!(out, "  {error}")?;
    }
    Ok(())
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Created => "created",
        RunStatus::Submitting => "submitting",
        RunStatus::UnknownSubmission => "unknown submission",
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Cancelled => "cancelled",
    }
}
