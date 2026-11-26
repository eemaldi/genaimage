mod artefacts;
mod completions;
mod config_local;
mod healthcheck;
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
    /// Loopback API origin for server-backed commands; defaults to the loopback
    /// address of GENAIMAGE_SERVER_PORT when set, else http://127.0.0.1:3000.
    #[arg(long, global = true, env = "GENAIMAGE_SERVER", value_name = "URL")]
    server: Option<String>,
    /// Configuration file for `serve`, or destination for `config init`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn server_url(&self) -> String {
        server_url(
            self.server.as_deref(),
            std::env::var("GENAIMAGE_SERVER_PORT").ok().as_deref(),
        )
    }
}

fn server_url(explicit: Option<&str>, port: Option<&str>) -> String {
    match (explicit, port.and_then(|port| port.parse::<u16>().ok())) {
        (Some(url), _) => url.to_owned(),
        (None, Some(port)) => format!("http://127.0.0.1:{port}"),
        (None, None) => DEFAULT_BASE_URL.to_owned(),
    }
}

#[derive(Subcommand)]
enum Command {
    /// Start the loopback HTTP server.
    Serve {
        /// Proxy portal requests to this loopback Vite HTTP origin.
        #[arg(long, value_name = "URL")]
        portal_dev_url: Option<String>,
    },
    /// Generate shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Open the server root in your default browser.
    Open {
        /// Print the server root URL instead of launching a browser.
        #[arg(long)]
        print: bool,
    },
    /// Check the server and its ComfyUI backend.
    Doctor,
    /// Exit non-zero unless the server answers its health endpoint (container probe).
    #[command(hide = true)]
    Healthcheck,
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
    if let Some(command) = server_commands::Command::from_cli(&cli.command) {
        let output = output::Output::new(cli.json);
        return match execute_server(&cli, command, output).await {
            Ok(outcome) => outcome.into(),
            Err(error) => output.report(&error),
        };
    }
    match &cli.command {
        Command::Healthcheck => healthcheck::execute(&cli).await,
        Command::Serve { portal_dev_url } => serve::execute(&cli, portal_dev_url.as_deref()).await,
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
    let client = Client::with_base_url(&cli.server_url())?;
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

#[cfg(test)]
mod tests {
    use super::server_url;

    #[test]
    fn server_url_prefers_the_explicit_origin_then_the_server_port() {
        assert_eq!(
            server_url(Some("http://127.0.0.1:1"), Some("9")),
            "http://127.0.0.1:1"
        );
        assert_eq!(server_url(None, Some("9000")), "http://127.0.0.1:9000");
        assert_eq!(server_url(None, Some("bogus")), "http://127.0.0.1:3000");
        assert_eq!(server_url(None, None), "http://127.0.0.1:3000");
    }
}
