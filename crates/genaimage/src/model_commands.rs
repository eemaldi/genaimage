use futures_util::StreamExt;
use genaimage_api::{
    client::{self, Client},
    CheckpointsResponse, LlmModelsResponse, LlmRoleModelState, ModelPull, ModelPullState,
};
use serde::Serialize;

use crate::{
    output::{Error, Output, Status},
    ModelsCommand,
};

pub(crate) async fn execute(
    client: &Client,
    output: Output,
    command: &ModelsCommand,
) -> Result<Status, Error> {
    match command {
        ModelsCommand::List => list(client, output).await,
        ModelsCommand::Pull { name } => pull(client, output, name).await,
    }
}

#[derive(Serialize)]
struct ModelsReport {
    llm: Option<LlmModelsResponse>,
    llm_error: Option<String>,
    checkpoints: Option<CheckpointsResponse>,
    checkpoints_error: Option<String>,
}

fn section<T>(result: Result<T, client::Error>) -> Result<(Option<T>, Option<String>), Error> {
    match result {
        Ok(value) => Ok((Some(value), None)),
        Err(client::Error::Problem(problem)) => Ok((
            None,
            Some(format!("{} {}", problem.detail, problem.recovery)),
        )),
        Err(error) => Err(error.into()),
    }
}

async fn list(client: &Client, output: Output) -> Result<Status, Error> {
    let (llm, llm_error) = section(client.llm_models().await)?;
    let (checkpoints, checkpoints_error) = section(client.checkpoints().await)?;
    let failed = llm_error.is_some() || checkpoints_error.is_some();
    let report = ModelsReport {
        llm,
        llm_error,
        checkpoints,
        checkpoints_error,
    };
    output.write(&report, |out| {
        match (&report.llm, &report.llm_error) {
            (Some(llm), _) => {
                writeln!(out, "LLM models ({} API):", source_name(llm))?;
                for model in &llm.models {
                    writeln!(
                        out,
                        "  {}  {}",
                        model.name,
                        model.size_bytes.map_or_else(String::new, human_bytes)
                    )?;
                }
                if llm.models.is_empty() {
                    writeln!(out, "  none installed")?;
                }
                for role in &llm.roles {
                    let state = match role.state {
                        LlmRoleModelState::Present => "installed",
                        LlmRoleModelState::Missing => "NOT INSTALLED",
                        LlmRoleModelState::NotConfigured => "not configured",
                    };
                    writeln!(
                        out,
                        "  {:?} role: {} ({state})",
                        role.role,
                        role.model.as_deref().unwrap_or("-")
                    )?;
                }
                if let Some(reason) = &llm.pull_unsupported_reason {
                    writeln!(out, "  Pull unavailable: {reason}")?;
                }
            }
            (None, Some(error)) => writeln!(out, "LLM models unavailable: {error}")?,
            (None, None) => {}
        }
        match (&report.checkpoints, &report.checkpoints_error) {
            (Some(checkpoints), _) => {
                writeln!(out, "ComfyUI checkpoints:")?;
                for name in &checkpoints.checkpoints {
                    writeln!(out, "  {name}")?;
                }
                if checkpoints.checkpoints.is_empty() {
                    writeln!(out, "  none installed")?;
                }
                for location in &checkpoints.locations {
                    writeln!(
                        out,
                        "  Put files in ({:?}): {}",
                        location.environment, location.directory
                    )?;
                    for step in &location.steps {
                        writeln!(out, "    {step}")?;
                    }
                }
                writeln!(out, "  {}", checkpoints.refresh_note)?;
            }
            (None, Some(error)) => writeln!(out, "ComfyUI checkpoints unavailable: {error}")?,
            (None, None) => {}
        }
        Ok(())
    })?;
    Ok(if failed {
        Status::ServerProblem
    } else {
        Status::Success
    })
}

fn source_name(llm: &LlmModelsResponse) -> &'static str {
    match llm.source {
        genaimage_api::LlmModelSource::Ollama => "Ollama",
        genaimage_api::LlmModelSource::Openai => "OpenAI-compatible",
    }
}

/// Decimal units, as Ollama reports sizes.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut whole = bytes;
    let mut remainder = 0;
    let mut unit = 0;
    while whole >= 1000 && unit < UNITS.len() - 1 {
        remainder = whole % 1000;
        whole /= 1000;
        unit += 1;
    }
    if unit == 0 {
        format!("{whole} B")
    } else {
        format!("{whole}.{} {}", remainder / 100, UNITS[unit])
    }
}

fn progress_line(pull: &ModelPull) -> String {
    match (pull.completed_bytes, pull.total_bytes) {
        (Some(done), Some(total)) if total > 0 => format!(
            "{}: {} / {} ({}%)",
            pull.status,
            human_bytes(done),
            human_bytes(total),
            u128::from(done.min(total)) * 100 / u128::from(total)
        ),
        _ => pull.status.clone(),
    }
}

/// Follows a pull to its end. Returns the terminal snapshot, or an error when
/// the pull cannot be observed any longer. A cut connection is not an outcome.
async fn follow(client: &Client, output: Output, first: ModelPull) -> Result<ModelPull, Error> {
    let id = first.id.clone();
    let mut last = first;
    let mut line_open = false;
    let mut connection_failures = 0;
    loop {
        match client.model_pull_events(&id).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(pull) => {
                            if !output.is_json() {
                                eprint!("\r\x1b[K{}", progress_line(&pull));
                                line_open = true;
                            }
                            last = pull;
                            if last.state != ModelPullState::Running {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            Err(client::Error::Problem(problem)) if problem.status == 404 => {
                return Err(Error::PullUnknown(id));
            }
            Err(error) => {
                connection_failures += 1;
                if connection_failures >= 5 {
                    return Err(error.into());
                }
            }
        }
        if last.state != ModelPullState::Running {
            break;
        }
        // The stream was cut before a terminal event: ask for the real state.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        match client.model_pull(&id).await {
            Ok(pull) => last = pull,
            Err(client::Error::Problem(problem)) if problem.status == 404 => {
                return Err(Error::PullUnknown(id));
            }
            Err(error) => {
                connection_failures += 1;
                if connection_failures >= 5 {
                    return Err(error.into());
                }
            }
        }
        if last.state != ModelPullState::Running {
            break;
        }
    }
    if line_open {
        eprintln!();
    }
    Ok(last)
}

async fn pull(client: &Client, output: Output, name: &str) -> Result<Status, Error> {
    let started = client.start_model_pull(name).await?;
    output.progress(format_args!(
        "Pulling {} (pull {}).",
        started.name, started.id
    ));
    let done = follow(client, output, started).await?;
    output.write(&done, |out| match done.state {
        ModelPullState::Succeeded => writeln!(
            out,
            "Pulled {} ({}).",
            done.name,
            done.total_bytes
                .map_or_else(|| "size unknown".into(), human_bytes)
        ),
        _ => Ok(()),
    })?;
    match done.state {
        ModelPullState::Succeeded => Ok(Status::Success),
        _ => Err(Error::PullFailed(Box::new(done))),
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(2_019_393_189), "2.0 GB");
        assert_eq!(human_bytes(5_000_000_000_000_000), "5000.0 TB");
    }
}
