use std::{path::PathBuf, process::ExitCode};

use genaimage_core::{HarnessConfig, HarnessError};

use crate::{output, Cli, Command, ConfigCommand};

pub(crate) async fn execute(cli: &Cli) -> ExitCode {
    let result = match &cli.command {
        Command::Serve => crate::serve::execute(cli).await,
        Command::Config {
            command: ConfigCommand::Init { force },
        } => {
            if cli.workspace.is_some() {
                Err(HarnessError::Configuration("--workspace is only supported by `genaimage serve`; use --config to choose where to write the example configuration".into()))
            } else {
                config_init(cli, *force)
            }
        }
        _ => unreachable!("server-backed commands are dispatched first"),
    };
    if let Err(error) = result {
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
