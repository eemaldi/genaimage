mod output;
mod run_commands;
mod server_commands;

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use genaimage_api::client::{Client, DEFAULT_BASE_URL};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{EventBus, ExtensionRegistry, Harness, HarnessConfig, HarnessError};
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
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Loopback API origin for server-backed commands.
    #[arg(long, global = true, env = "GENAIMAGE_SERVER", default_value = DEFAULT_BASE_URL, value_name = "URL")]
    server: String,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the loopback HTTP server.
    Serve,
    /// Check the server and its ComfyUI backend.
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
        id: String,
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
    /// Show settings from the running server.
    Show,
}

#[derive(Subcommand)]
enum WorkflowCommand {
    List,
    Validate {
        id: String,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long = "set", value_name = "NAME=JSON")]
        parameters: Vec<String>,
    },
    Inspect {
        id: String,
    },
}

#[derive(Args)]
struct RunCommand {
    workflow: String,
    #[arg(long)]
    preset: Option<String>,
    #[arg(long = "set", value_name = "NAME=JSON")]
    parameters: Vec<String>,
    /// Upload an explicitly selected local reference image.
    #[arg(long = "image", value_name = "NAME=PATH")]
    images: Vec<String>,
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
        /// Return the next page using a cursor from a previous list.
        #[arg(long)]
        cursor: Option<String>,
    },
    Show {
        run_id: String,
        #[arg(long, default_value_t = 100)]
        events: usize,
        /// Return the next page of output metadata.
        #[arg(long)]
        output_cursor: Option<String>,
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
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(command) = server_commands::Command::from_cli(&cli.command) {
        let output = output::Output::new(cli.json);
        return match execute_server(&cli, command, output).await {
            Ok(outcome) => outcome.into(),
            Err(error) => output.report(&error),
        };
    }
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
    }
    if let Err(error) = execute_local(&cli).await {
        if cli.json {
            eprintln!(
                "{}",
                serde_json::json!({ "status": "error", "error": error.to_string() })
            );
        } else {
            eprintln!("Error: {error}");
        }
        return output::Status::LocalFailure.into();
    }
    ExitCode::SUCCESS
}

async fn execute_server(
    cli: &Cli,
    command: server_commands::Command<'_>,
    output: output::Output,
) -> Result<output::Status, output::Error> {
    let client = Client::with_base_url(&cli.server)?;
    command.execute(&client, output).await
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

async fn execute_local(cli: &Cli) -> Result<(), HarnessError> {
    match &cli.command {
        Command::Serve => {
            let harness = open_harness(cli).await?;
            genaimage_server::serve(genaimage_server::AppState::new(harness.into())).await?;
        }
        Command::Config {
            command: ConfigCommand::Init { force },
        } => config_init(cli, *force)?,
        _ => unreachable!("server-backed commands are dispatched first"),
    }
    Ok(())
}

fn config_init(cli: &Cli, force: bool) -> Result<(), HarnessError> {
    let path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("genaimage.toml"));
    if path.exists() && !force {
        return Err(HarnessError::Configuration(format!(
            "'{}' already exists; use --force to replace it",
            path.display()
        )));
    }
    std::fs::write(&path, HarnessConfig::example())?;
    let report = serde_json::json!({ "path": path, "created": true });
    if cli.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("Created configuration file '{}'.", path.display());
    }
    Ok(())
}

fn parse_parameters(values: &[String]) -> Result<BTreeMap<String, Value>, String> {
    let mut parameters = BTreeMap::new();
    for value in values {
        let (name, raw) = value
            .split_once('=')
            .ok_or_else(|| format!("parameter override '{value}' must use NAME=JSON form"))?;
        if name.trim().is_empty() {
            return Err("parameter name cannot be empty".to_owned());
        }
        let parsed = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
        parameters.insert(name.to_owned(), parsed);
    }
    Ok(parameters)
}
