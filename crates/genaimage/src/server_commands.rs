use genaimage_api::{
    client::Client, SettingSource, WorkflowNodeCheckStatus, WorkflowValidationRequest,
};

use crate::{
    output::{Error, Output, Status},
    ConfigCommand, WorkflowCommand,
};

pub(crate) enum Command<'a> {
    Doctor,
    Workflow(&'a WorkflowCommand),
    Presets(&'a str),
    ConfigShow,
    Open { print: bool },
    Run(&'a crate::RunCommand),
    Jobs(&'a crate::JobsCommand),
    Models(&'a crate::ModelsCommand),
}

impl<'a> Command<'a> {
    pub(crate) fn from_cli(command: &'a crate::Command) -> Option<Self> {
        match command {
            crate::Command::Doctor => Some(Self::Doctor),
            crate::Command::Open { print } => Some(Self::Open { print: *print }),
            crate::Command::Workflow { command } => Some(Self::Workflow(command)),
            crate::Command::Presets { id } => Some(Self::Presets(id)),
            crate::Command::Config {
                command: ConfigCommand::Show,
            } => Some(Self::ConfigShow),
            crate::Command::Run(command) => Some(Self::Run(command)),
            crate::Command::Jobs { command } => Some(Self::Jobs(command)),
            crate::Command::Models { command } => Some(Self::Models(command)),
            _ => None,
        }
    }

    pub(crate) async fn execute(self, client: &Client, output: Output) -> Result<Status, Error> {
        match self {
            Self::Run(command) => return crate::run_commands::run(client, output, command).await,
            Self::Jobs(command) => return crate::run_commands::jobs(client, output, command).await,
            Self::Models(command) => {
                return crate::model_commands::execute(client, output, command).await
            }
            Self::Doctor => return crate::doctor::run(client, output).await,
            Self::Workflow(WorkflowCommand::List) => {
                let catalogue = client.workflows().await?;
                output.write(&catalogue, |out| {
                    if catalogue.workflows.is_empty() {
                        writeln!(out, "No workflows found.")?;
                    }
                    for workflow in &catalogue.workflows {
                        writeln!(
                            out,
                            "{}  {} ({})",
                            workflow.id, workflow.name, workflow.version
                        )?;
                    }
                    for diagnostic in &catalogue.diagnostics {
                        writeln!(out, "Skipped {}: {}", diagnostic.path, diagnostic.error)?;
                    }
                    Ok(())
                })?;
            }
            Self::Workflow(WorkflowCommand::Inspect { id }) => {
                let workflow = client.workflow(id).await?;
                output.write(&workflow, |out| {
                    writeln!(
                        out,
                        "{} ({}, version {})",
                        workflow.title, workflow.id, workflow.version
                    )?;
                    if let Some(description) = &workflow.description {
                        writeln!(out, "{description}")?;
                    }
                    writeln!(out, "Workflow: {}", workflow.workflow)?;
                    for (name, parameter) in &workflow.parameters {
                        write!(
                            out,
                            "  {name} ({}{}) → {}.{}",
                            parameter.kind,
                            if parameter.required { ", required" } else { "" },
                            parameter.node,
                            parameter.input
                        )?;
                        if let Some(default) = &parameter.default {
                            write!(out, " [default: {default}]")?;
                        }
                        writeln!(out, "  {}", parameter.description)?;
                    }
                    for output in &workflow.outputs {
                        writeln!(out, "  Output: {} ({})", output.node, output.kind)?;
                    }
                    if !workflow.presets.is_empty() {
                        let presets = workflow
                            .presets
                            .keys()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        writeln!(out, "Presets: {}", presets.join(", "))?;
                    }
                    Ok(())
                })?;
            }
            Self::Workflow(WorkflowCommand::Validate {
                id,
                preset,
                parameters,
            }) => {
                let request = WorkflowValidationRequest {
                    workflow_revision: None,
                    preset: preset.clone(),
                    parameters: crate::parse_parameters(parameters).map_err(Error::Input)?,
                };
                let result = client.validate_workflow(id, &request).await?;
                output.write(&result, |out| {
                    if result.valid {
                        return writeln!(out, "Workflow '{id}' is valid.");
                    }
                    writeln!(out, "Workflow '{id}' did not pass validation.")?;
                    for error in &result.parameter_errors {
                        writeln!(out, "  {}: {}", error.parameter, error.reason)?;
                    }
                    match result.node_check.status {
                        WorkflowNodeCheckStatus::Available => Ok(()),
                        WorkflowNodeCheckStatus::Missing => writeln!(
                            out,
                            "  Missing ComfyUI node classes: {}",
                            result.node_check.missing_classes.join(", ")
                        ),
                        WorkflowNodeCheckStatus::Unavailable => writeln!(
                            out,
                            "  ComfyUI node check unavailable: {}",
                            result
                                .node_check
                                .reason
                                .as_deref()
                                .unwrap_or("no reason given")
                        ),
                    }
                })?;
                if !result.valid {
                    return Ok(Status::NegativeResult);
                }
            }
            Self::Presets(id) => {
                let presets = client.workflow(id).await?.presets;
                output.write(&presets, |out| {
                    if presets.is_empty() {
                        writeln!(out, "No presets for workflow '{id}'.")?;
                    }
                    for (name, parameters) in &presets {
                        writeln!(out, "{name}:")?;
                        for (parameter, value) in parameters {
                            writeln!(out, "  {parameter} = {value}")?;
                        }
                    }
                    Ok(())
                })?;
            }
            Self::ConfigShow => {
                let config = client.settings().await?;
                output.write(&config, |out| {
                    writeln!(out, "Server configuration file: {}", config.config_file)?;
                    for (name, setting) in &config.settings {
                        write!(out, "{name} = {}", setting.value)?;
                        match &setting.source {
                            SettingSource::Default => write!(out, " (default)")?,
                            SettingSource::File { file } => write!(out, " (file: {file})")?,
                            SettingSource::Env { variable } => {
                                write!(out, " (environment: {variable})")?
                            }
                        }
                        if setting.read_only {
                            write!(out, " [read-only]")?;
                        }
                        if let Some(active) = &setting.active_value {
                            write!(out, " [active: {active}]")?;
                        }
                        writeln!(out)?;
                    }
                    if !config.restart_required.is_empty() {
                        writeln!(
                            out,
                            "Restart required for: {}",
                            config.restart_required.join(", ")
                        )?;
                    }
                    Ok(())
                })?;
            }
            Self::Open { print } => {
                return crate::open::run(client, output, print).await;
            }
        }
        Ok(Status::Success)
    }
}
