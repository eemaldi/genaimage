use std::{process::ExitCode, sync::Arc, time::Duration};

use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{EventBus, ExtensionRegistry, Harness, HarnessConfig, HarnessError, LlmRoles};
use genaimage_llm::{OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions};
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
    let (harness, llm_roles) = open_harness(cli).await?;
    let mut state = genaimage_server::AppState::new(harness.into()).with_llm_roles(llm_roles);
    if let Some(proxy) = portal_dev {
        state = state.with_portal_dev(proxy);
    }
    genaimage_server::serve(state).await?;
    Ok(())
}

async fn open_harness(cli: &Cli) -> Result<(Harness, LlmRoles), HarnessError> {
    let config = HarnessConfig::load_with_config(
        cli.workspace
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(".")),
        cli.config.as_deref(),
    )?;
    let mut registry = ExtensionRegistry::from_config(&config, ComfyUiClient::new)?;
    if !config.llm.base_url.is_empty() {
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
            base_url: config.llm.base_url.clone(),
            api_key: config.llm.api_key.clone(),
            allowed_hosts: config.llm.allowed_hosts.clone(),
            connect_timeout: Duration::from_secs(config.llm.connect_timeout_secs),
            first_byte_timeout: Duration::from_secs(config.llm.request_timeout_secs),
            idle_timeout: Duration::from_secs(config.llm.idle_timeout_secs),
        })?;
        registry.register_llm_provider(genaimage_core::LLM_PROVIDER, Arc::new(provider))?;
    }
    let llm_roles = LlmRoles::new(
        &config.llm,
        registry.llm_provider(genaimage_core::LLM_PROVIDER),
    );
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
    Ok((harness, llm_roles))
}
