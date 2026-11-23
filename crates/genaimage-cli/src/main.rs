use std::{collections::BTreeMap, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use genaimage_core::{Harness, HarnessConfig, HarnessError, RunProgress, RunRequest, RunStatus};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "genaimage",
    version,
    about = "A controllable ComfyUI workflow harness"
)]
struct Cli {
    #[arg(long, default_value = ".", global = true)]
    workspace: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Doctor,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Presets {
        workflow: String,
    },
    Run(RunCommand),
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    Init {
        #[arg(long)]
        force: bool,
    },
    Show,
}

#[derive(Subcommand)]
enum WorkflowCommand {
    List,
    Validate { workflow: String },
    Inspect { workflow: String },
}

#[derive(Args)]
struct RunCommand {
    workflow: String,
    #[arg(long)]
    preset: Option<String>,
    #[arg(long = "set", value_name = "NAME=JSON")]
    parameters: Vec<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    wait: bool,
}

#[derive(Subcommand)]
enum JobsCommand {
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Show {
        run_id: String,
        #[arg(long, default_value_t = 100)]
        events: usize,
    },
    Reconcile {
        run_id: Option<String>,
    },
    Wait {
        run_id: String,
    },
    Cancel {
        run_id: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .without_time()
        .init();
    let cli = Cli::parse();
    if let Err(error) = execute(&cli).await {
        if cli.json {
            eprintln!(
                "{}",
                serde_json::json!({ "status": "error", "error": error.to_string() })
            );
        } else {
            eprintln!("Error: {error}");
        }
        std::process::exit(1);
    }
}

async fn execute(cli: &Cli) -> Result<(), HarnessError> {
    match &cli.command {
        Command::Config { command } => return config_command(cli, command),
        Command::Doctor => {
            let harness = Harness::open(&cli.workspace)?;
            let health = harness.health().await?;
            output(
                cli,
                &serde_json::json!({
                    "configuration": harness.config(),
                    "comfyui": health
                }),
            )?;
        }
        Command::Workflow { command } => workflow_command(cli, command)?,
        Command::Presets { workflow } => {
            let harness = Harness::open(&cli.workspace)?;
            let loaded = harness.load_workflow(workflow)?;
            output(cli, &loaded.manifest.presets)?;
        }
        Command::Run(command) => run_command(cli, command).await?,
        Command::Jobs { command } => jobs_command(cli, command).await?,
    }
    Ok(())
}

fn config_command(cli: &Cli, command: &ConfigCommand) -> Result<(), HarnessError> {
    let path = cli.workspace.join("genaimage.toml");
    match command {
        ConfigCommand::Init { force } => {
            if path.exists() && !force {
                return Err(HarnessError::Configuration(format!(
                    "'{}' already exists; use --force to replace it",
                    path.display()
                )));
            }
            std::fs::write(&path, HarnessConfig::example())?;
            output(cli, &serde_json::json!({ "path": path, "created": true }))?;
        }
        ConfigCommand::Show => {
            let harness = Harness::open(&cli.workspace)?;
            output(cli, harness.config())?;
        }
    }
    Ok(())
}

fn workflow_command(cli: &Cli, command: &WorkflowCommand) -> Result<(), HarnessError> {
    let harness = Harness::open(&cli.workspace)?;
    match command {
        WorkflowCommand::List => output(cli, &harness.discover_workflows()?)?,
        WorkflowCommand::Validate { workflow } => {
            let loaded = harness.load_workflow(workflow)?;
            output(
                cli,
                &serde_json::json!({
                    "valid": true,
                    "manifest": loaded.manifest_path,
                    "workflow": loaded.workflow_path,
                    "workflow_sha256": loaded.workflow_sha256
                }),
            )?;
        }
        WorkflowCommand::Inspect { workflow } => {
            let loaded = harness.load_workflow(workflow)?;
            output(
                cli,
                &serde_json::json!({
                    "manifest": loaded.manifest,
                    "parameters": loaded.parameter_summary(),
                    "workflow_sha256": loaded.workflow_sha256
                }),
            )?;
        }
    }
    Ok(())
}

async fn run_command(cli: &Cli, command: &RunCommand) -> Result<(), HarnessError> {
    let harness = Harness::open(&cli.workspace)?;
    let parameters = parse_parameters(&command.parameters)?;
    let run = harness
        .submit(RunRequest {
            workflow: command.workflow.clone(),
            preset: command.preset.clone(),
            parameters,
            version: command.version.clone(),
        })
        .await?;
    output(cli, &run)?;
    if command.wait && run.status.is_trackable() {
        let run_id = run.id.clone();
        let (sender, mut receiver) = mpsc::unbounded_channel::<RunProgress>();
        let waiting_harness = harness.clone();
        let waiter =
            tokio::spawn(async move { waiting_harness.wait_for_run(&run_id, Some(sender)).await });
        while let Some(progress) = receiver.recv().await {
            if !cli.json {
                print_progress(&progress);
            }
        }
        let completed = waiter
            .await
            .map_err(|error| HarnessError::ComfyUi(format!("wait task failed: {error}")))??;
        output(cli, &completed)?;
    }
    if run.status == RunStatus::UnknownSubmission {
        return Err(HarnessError::ComfyUi(format!(
            "submission of run '{}' is ambiguous; use 'jobs reconcile' before retrying",
            run.id
        )));
    }
    Ok(())
}

async fn jobs_command(cli: &Cli, command: &JobsCommand) -> Result<(), HarnessError> {
    let harness = Harness::open(&cli.workspace)?;
    match command {
        JobsCommand::List { limit } => output(cli, &harness.list_runs(*limit)?)?,
        JobsCommand::Show { run_id, events } => output(
            cli,
            &serde_json::json!({
                "run": harness.get_run(run_id)?,
                "events": harness.run_events(run_id, *events)?,
                "outputs": harness.repository().outputs(run_id)?
            }),
        )?,
        JobsCommand::Reconcile { run_id } => {
            if let Some(run_id) = run_id {
                output(cli, &harness.reconcile(run_id).await?)?;
            } else {
                output(cli, &harness.reconcile_all().await?)?;
            }
        }
        JobsCommand::Wait { run_id } => {
            let completed = harness.wait_for_run(run_id, None).await?;
            output(cli, &completed)?;
        }
        JobsCommand::Cancel { run_id } => output(cli, &harness.cancel(run_id).await?)?,
    }
    Ok(())
}

fn output<T: Serialize>(cli: &Cli, value: &T) -> Result<(), HarnessError> {
    let rendered = if cli.json {
        serde_json::to_string(value)?
    } else {
        serde_json::to_string_pretty(value)?
    };
    println!("{rendered}");
    Ok(())
}

fn parse_parameters(values: &[String]) -> Result<BTreeMap<String, Value>, HarnessError> {
    let mut parameters = BTreeMap::new();
    for value in values {
        let (name, raw) = value.split_once('=').ok_or_else(|| {
            HarnessError::Configuration(format!(
                "parameter override '{value}' must use NAME=JSON form"
            ))
        })?;
        if name.trim().is_empty() {
            return Err(HarnessError::Configuration(
                "parameter name cannot be empty".to_owned(),
            ));
        }
        let parsed = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
        parameters.insert(name.to_owned(), parsed);
    }
    Ok(parameters)
}

fn print_progress(progress: &RunProgress) {
    if let Some(event) = &progress.event {
        let suffix = event
            .value
            .zip(event.maximum)
            .map(|(value, maximum)| format!(" {value}/{maximum}"))
            .unwrap_or_default();
        eprintln!("{}{}", event.event_type, suffix);
    } else {
        eprintln!("{}: {}", progress.run.id, progress.run.status.as_str());
    }
}
