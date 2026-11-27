use std::{process::ExitCode, sync::Arc, time::Duration};

use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    config::{SinkConfig, SinkKind},
    sinks::{self, JsonlSink, SinkRuntime, SinkStatus},
    EventBus, ExtensionRegistry, Harness, HarnessConfig, HarnessError, LlmRoles, ResolvedConfig,
};
use genaimage_llm::{OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions};
use genaimage_webhook::{WebhookOptions, WebhookSink};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
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
    let (harness, llm_roles, assistant_tools, sink_tasks) = open_harness(cli).await?;
    let mut state = genaimage_server::AppState::new(harness.into())
        .with_llm_roles(llm_roles)
        .with_assistant_tools(assistant_tools)?;
    if let Some(proxy) = portal_dev {
        state = state.with_portal_dev(proxy);
    }
    let served = genaimage_server::serve(state).await;
    sink_tasks.finish().await;
    served?;
    Ok(())
}

/// Event sink tasks, stopped after the server has drained its own work.
struct SinkTasks {
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl SinkTasks {
    /// Sinks drain for their own deadline; this wait is bounded just above it.
    async fn finish(self) {
        self.shutdown.cancel();
        self.tracker.close();
        let deadline = sinks::DEFAULT_DRAIN_DEADLINE + Duration::from_secs(2);
        if tokio::time::timeout(deadline, self.tracker.wait())
            .await
            .is_err()
        {
            eprintln!("event sinks did not stop within {} s", deadline.as_secs());
        }
    }
}

fn start_sinks(
    config: &ResolvedConfig,
    registry: &mut ExtensionRegistry,
    bus: &EventBus,
) -> Result<SinkTasks, HarnessError> {
    let tasks = SinkTasks {
        shutdown: CancellationToken::new(),
        tracker: TaskTracker::new(),
    };
    for sink in &config.sinks {
        let status = SinkStatus::new(&sink.name, sink.kind.as_str());
        let implementation: Arc<dyn genaimage_core::EventSink> = match sink.kind {
            SinkKind::Jsonl => Arc::new(JsonlSink::new(sinks::jsonl_path(
                config,
                sink.path.as_deref().unwrap_or_default(),
            ))?),
            SinkKind::Webhook => Arc::new(WebhookSink::new(
                WebhookOptions::from_config(sink, sinks::dead_letter_path(config, &sink.name))?,
                Arc::clone(&status),
            )?),
        };
        registry.register_event_sink(&sink.name, Arc::clone(&implementation))?;
        let selection: SinkConfig = sink.clone();
        sinks::spawn_sink(
            bus,
            implementation,
            status,
            move |event| selection.wants(event),
            SinkRuntime::default(),
            &tasks.shutdown,
            &tasks.tracker,
        );
    }
    Ok(tasks)
}

async fn open_harness(
    cli: &Cli,
) -> Result<
    (
        Harness,
        LlmRoles,
        Vec<Arc<dyn genaimage_core::AssistantTool>>,
        SinkTasks,
    ),
    HarnessError,
> {
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
    llm_roles.refresh_in_background();
    let backend = registry
        .image_backend(genaimage_core::extensions::COMFYUI_BACKEND)
        .ok_or_else(|| HarnessError::Configuration("no configured ComfyUI backend".into()))?;
    let bus = EventBus::default();
    let sink_tasks = start_sinks(&config, &mut registry, &bus)?;
    let harness = Harness::with_event_bus_async(config, backend, bus.clone()).await?;
    let builtins = genaimage_core::assistant::tools::builtin_assistant_tools(
        std::sync::Arc::new(harness.clone()),
        llm_roles.clone(),
    );
    for tool in builtins {
        registry.register_assistant_tool(tool)?;
    }
    Ok((harness, llm_roles, registry.assistant_tools(), sink_tasks))
}
