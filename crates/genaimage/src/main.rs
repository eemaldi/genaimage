use std::{collections::BTreeMap, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    Event, EventBus, ExtensionRegistry, Harness, HarnessConfig, HarnessError, ReceiveError,
    RunRequest, RunStatus,
};
use serde::Serialize;
use serde_json::Value;
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
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the loopback HTTP server.
    Serve,
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
    /// Request cancellation; a running ComfyUI job uses a global interrupt.
    #[command(
        long_about = "Request cancellation. ComfyUI uses a global interrupt for running jobs, so if the target finishes before the request arrives, it may interrupt a different prompt. A cancellation remains unconfirmed without evidence for the target prompt."
    )]
    Cancel {
        run_id: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Serve) {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::builder()
                    .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                    .from_env_lossy(),
            )
            .with_writer(std::io::stderr)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_target(false)
            .without_time()
            .init();
    }
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

async fn open_harness(cli: &Cli) -> Result<Harness, HarnessError> {
    let config = HarnessConfig::load_with_config(&cli.workspace, cli.config.as_deref())?;
    let registry = ExtensionRegistry::from_config(&config, ComfyUiClient::new)?;
    let backend = registry
        .image_backend(genaimage_core::extensions::COMFYUI_BACKEND)
        .ok_or_else(|| HarnessError::Configuration("no configured ComfyUI backend".into()))?;
    let bus = EventBus::default();
    let harness = Harness::with_event_bus_async(config, backend, bus.clone()).await?;
    for (name, task) in registry.forward_event_sinks(&bus) {
        tokio::spawn(async move {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("event sink '{name}' failed: {error}"),
                Err(error) => eprintln!("event sink '{name}' task failed: {error}"),
            }
        });
    }
    Ok(harness)
}

async fn execute(cli: &Cli) -> Result<(), HarnessError> {
    match &cli.command {
        Command::Config { command } => return config_command(cli, command).await,
        Command::Serve => {
            let harness = open_harness(cli).await?;
            genaimage_server::serve(genaimage_server::AppState::new(harness.into())).await?;
        }
        Command::Doctor => {
            let harness = open_harness(cli).await?;
            let health = harness.health().await?;
            output(
                cli,
                &serde_json::json!({
                    "configuration": harness.config(),
                    "comfyui": health
                }),
            )?;
        }
        Command::Workflow { command } => workflow_command(cli, command).await?,
        Command::Presets { workflow } => {
            let harness = open_harness(cli).await?;
            let loaded = harness.load_workflow(workflow)?;
            output(cli, &loaded.manifest.presets)?;
        }
        Command::Run(command) => run_command(cli, command).await?,
        Command::Jobs { command } => jobs_command(cli, command).await?,
    }
    Ok(())
}

async fn config_command(cli: &Cli, command: &ConfigCommand) -> Result<(), HarnessError> {
    let path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("genaimage.toml"));
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
            let config = HarnessConfig::load_with_config(&cli.workspace, cli.config.as_deref())?;
            output(cli, &config)?;
        }
    }
    Ok(())
}

async fn workflow_command(cli: &Cli, command: &WorkflowCommand) -> Result<(), HarnessError> {
    let harness = open_harness(cli).await?;
    match command {
        WorkflowCommand::List => {
            let discovery = harness.discover_workflows()?;
            for diagnostic in discovery.diagnostics {
                eprintln!(
                    "warning: skipping manifest '{}': {}",
                    diagnostic.path.display(),
                    diagnostic.error
                );
            }
            output(cli, &discovery.manifests)?;
        }
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
    let harness = open_harness(cli).await?;
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
        let completed = if cli.json {
            let run_id = run.id.clone();
            let waiting_harness = harness.clone();
            tokio::spawn(async move { waiting_harness.wait_for_run(&run_id).await })
                .await
                .map_err(|error| HarnessError::ComfyUi(format!("wait task failed: {error}")))??
        } else {
            let mut receiver = harness.events().subscribe_run_progress(&run.id);
            let run_id = run.id.clone();
            let waiting_harness = harness.clone();
            let mut waiter =
                tokio::spawn(async move { waiting_harness.wait_for_run(&run_id).await });
            loop {
                tokio::select! {
                    biased;
                    event = receiver.recv() => match event {
                        Ok(envelope) => {
                            match envelope.event {
                                Event::RunProgress { run: progress_run, backend_event } if progress_run.id == run.id => {
                                    print_progress(&progress_run, backend_event.as_ref());
                                }
                                _ => {}
                            }
                        }
                        // This stream carries every progress update and maintains no
                        // lifecycle state; the final poll supplies the current run.
                        Err(ReceiveError::ResyncRequired(_)) => {},
                        Err(ReceiveError::Closed) => {
                            break waiter.await.map_err(|error| HarnessError::ComfyUi(format!("wait task failed: {error}")))??;
                        }
                    },
                    result = &mut waiter => break result.map_err(|error| HarnessError::ComfyUi(format!("wait task failed: {error}")))??,
                }
            }
        };
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
    let harness = open_harness(cli).await?;
    match command {
        JobsCommand::List { limit } => {
            output(cli, &harness.repository().as_async().list(*limit).await?)?
        }
        JobsCommand::Show { run_id, events } => output(
            cli,
            &serde_json::json!({
                "run": harness.repository().as_async().get(run_id).await?,
                "events": harness.repository().as_async().events(run_id, *events).await?,
                "outputs": harness.repository().as_async().outputs(run_id).await?
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
            let completed = harness.wait_for_run(run_id).await?;
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

fn print_progress(
    run: &genaimage_core::RunRecord,
    event: Option<&genaimage_core::image_backend::BackendEvent>,
) {
    if let Some(event) = event {
        let suffix = event
            .value
            .zip(event.maximum)
            .map(|(value, maximum)| format!(" {value}/{maximum}"))
            .unwrap_or_default();
        eprintln!("{}{}", event.event_type, suffix);
    } else {
        eprintln!("{}: {}", run.id, run.status.as_str());
    }
}
