use std::{path::PathBuf, process::ExitCode};

use genaimage_core::{HarnessConfig, HarnessError};

use crate::{output::Output, Cli};

pub(crate) fn execute(cli: &Cli, force: bool) -> ExitCode {
    let result = if cli.workspace.is_some() {
        Err(HarnessError::Configuration("--workspace is only supported by `genaimage serve`; use --config to choose where to write the example configuration".into()))
    } else {
        config_init(cli, force)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => Output::new(cli.json).report_local(&error),
    }
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
