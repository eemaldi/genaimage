use std::process::ExitCode;

use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{EventBus, ExtensionRegistry, Harness, HarnessConfig, HarnessError};
use tracing_subscriber::EnvFilter;

use crate::{output::Output, Cli};

pub(crate) async fn execute(cli: &Cli, portal_dev_url: Option<&str>) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .json()
        .init();
    match start(cli, portal_dev_url).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => Output::new(cli.json).report_local(&error),
    }
}

async fn start(cli: &Cli, portal_dev_url: Option<&str>) -> Result<(), HarnessError> {
    let portal_dev = portal_dev_url
        .map(genaimage_server::PortalDevProxy::new)
        .transpose()
        .map_err(|error| HarnessError::Configuration(error.to_string()))?;
    let harness = open_harness(cli).await?;
    let mut state = genaimage_server::AppState::new(harness.into());
    if let Some(proxy) = portal_dev {
        state = state.with_portal_dev(proxy);
    }
    genaimage_server::serve(state).await?;
    Ok(())
}

async fn open_harness(cli: &Cli) -> Result<Harness, HarnessError> {
    let config = HarnessConfig::load_with_config(
        cli.workspace
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(".")),
        cli.config.as_deref(),
    )?;
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
