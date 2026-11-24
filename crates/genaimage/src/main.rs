mod artefacts;
mod completions;
mod config_local;
mod open;
mod output;
mod run_commands;
mod serve;
mod server_commands;

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use genaimage_api::client::{Client, DEFAULT_BASE_URL};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "genaimage",
    version,
    about = "A controllable ComfyUI workflow harness"
)]
struct Cli {
    /// Workspace for `serve` only (defaults to the current directory).
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Loopback API origin for server-backed commands.
    #[arg(long, global = true, env = "GENAIMAGE_SERVER", default_value = DEFAULT_BASE_URL, value_name = "URL")]
    server: String,
    /// Configuration file for `serve`, or destination for `config init`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the loopback HTTP server.
    Serve,
    /// Generate shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Open the server root in your default browser.
    ///
    /// The embedded portal is planned; this opens the server root for now.
    Open {
        /// Print the server root URL instead of launching a browser.
        #[arg(long)]
        print: bool,
    },
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
    /// Maximum observation time in seconds (default: 600).
    #[arg(long, requires = "wait", value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout: Option<u64>,
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
        /// Maximum observation time in seconds.
        #[arg(long, default_value_t = run_commands::DEFAULT_WAIT_SECONDS, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout: u64,
    },
    /// Download the original outputs into a local directory.
    Download {
        run_id: String,
        #[arg(long, value_name = "DIR")]
        dir: PathBuf,
    },
    /// Print the run manifest, or save it to a new local file.
    Manifest {
        run_id: String,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
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
    if let Command::Completions { shell } = &cli.command {
        completions::generate(*shell);
        return ExitCode::SUCCESS;
    }
    if let Some(command) = server_commands::Command::from_cli(&cli.command, &cli.server) {
        let output = output::Output::new(cli.json);
        return match execute_server(&cli, command, output).await {
            Ok(outcome) => outcome.into(),
            Err(error) => output.report(&error),
        };
    }
    match &cli.command {
        Command::Serve => serve::execute(&cli).await,
        Command::Config {
            command: ConfigCommand::Init { force },
        } => config_local::execute(&cli, *force),
        _ => unreachable!("server-backed commands are dispatched first"),
    }
}

async fn execute_server(
    cli: &Cli,
    command: server_commands::Command<'_>,
    output: output::Output,
) -> Result<output::Status, output::Error> {
    if cli.workspace.is_some()
        || cli.config.is_some()
        || std::env::var_os("GENAIMAGE_CONFIG").is_some()
    {
        return Err(output::Error::Input("--workspace, --config and GENAIMAGE_CONFIG are only for selecting the server's workspace or configuration. Pass the options (or set GENAIMAGE_CONFIG) when running `genaimage serve`, unset GENAIMAGE_CONFIG for this command, and select the server with --server.".into()));
    }
    if std::env::var_os("GENAIMAGE_PROFILE").is_some() {
        return Err(output::Error::Input("GENAIMAGE_PROFILE is no longer supported. Unset it, migrate the selected profile to [comfyui] in the configuration used by `genaimage serve`, and select the server with --server.".into()));
    }
    let client = Client::with_base_url(&cli.server)?;
    command.execute(&client, output).await
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
        if parameters.contains_key(name) {
            return Err(format!("Parameter '{name}' was specified more than once."));
        }
        let parsed = serde_json::from_str(raw).map_err(|error| {
            format!("Invalid JSON for parameter '{name}': {error}. Quote string values as JSON.")
        })?;
        parameters.insert(name.to_owned(), parsed);
    }
    Ok(parameters)
}
