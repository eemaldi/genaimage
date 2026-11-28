//! Minimal extensions that compile against the real core traits. See docs/extending.md.
//! Run with `cargo run -p genaimage-core --example extending`.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use genaimage_core::{
    async_trait,
    image_backend::{
        BackendHealth, EventMonitor, HistoryOutcome, HistorySnapshot, OutputReference,
        PromptResponse, QueueSnapshot, UploadResponse,
    },
    sinks::SinkRuntime,
    AssistantTool, ChatRequest, ChatStream, Event, EventBus, EventSink, ExtensionRegistry,
    HarnessError, ImageBackend, LlmModel, LlmProvider, Result, SinkNotification, ToolContext,
};
use serde_json::{json, Value};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const EXAMPLE: &str = "example";

fn unavailable(operation: &str) -> HarnessError {
    HarnessError::Backend {
        backend: EXAMPLE.to_owned(),
        message: format!("{operation} is not available in this example backend"),
    }
}

struct OfflineBackend;

#[async_trait]
impl ImageBackend for OfflineBackend {
    fn name(&self) -> &str {
        EXAMPLE
    }

    fn base_url(&self) -> &str {
        ""
    }

    async fn health(&self) -> Result<BackendHealth> {
        Err(unavailable("health"))
    }

    async fn object_info(&self) -> Result<Value> {
        Err(unavailable("object_info"))
    }

    async fn queue(&self) -> Result<QueueSnapshot> {
        Err(unavailable("queue"))
    }

    async fn submit(&self, _: &Value, _: &str, _: &str) -> Result<PromptResponse> {
        Err(unavailable("submit"))
    }

    async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot> {
        Ok(HistorySnapshot {
            prompt_id: prompt_id.to_owned(),
            outcome: HistoryOutcome::Unresolved,
            outputs: Vec::new(),
            raw: Value::Null,
        })
    }

    async fn delete_queued(&self, _: &str) -> Result<Value> {
        Err(unavailable("delete_queued"))
    }

    async fn prepare_interrupt(&self, _: &str) -> Result<()> {
        Err(unavailable("targeted interrupt"))
    }

    async fn interrupt(&self, _: &str) -> Result<Value> {
        Err(unavailable("interrupt"))
    }

    async fn upload_image(&self, _: &Path) -> Result<UploadResponse> {
        Err(unavailable("upload_image"))
    }

    async fn download_output(&self, _: &OutputReference) -> Result<Vec<u8>> {
        Err(unavailable("download_output"))
    }

    async fn monitor_events(&self, _: &str) -> Result<EventMonitor> {
        Err(unavailable("monitor_events"))
    }
}

struct NoModelProvider;

#[async_trait]
impl LlmProvider for NoModelProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        Err(HarnessError::Configuration(format!(
            "model '{}' is not served by the example provider",
            request.model
        )))
    }

    async fn list_models(&self) -> Result<Vec<LlmModel>> {
        Ok(Vec::new())
    }
}

struct RecordNoteTool;

#[async_trait]
impl AssistantTool for RecordNoteTool {
    fn name(&self) -> &str {
        "record_note"
    }

    fn description(&self) -> &str {
        "Record a short operator note. Needs the user's confirmation before it runs."
    }

    fn json_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "note": {"type": "string", "minLength": 1, "maxLength": 500}
            },
            "required": ["note"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn preview(&self, arguments: &Value) -> Result<Option<Value>> {
        Ok(Some(arguments.clone()))
    }

    async fn invoke(&self, context: ToolContext, arguments: Value) -> Result<Value> {
        if context.confirmation_id.is_none() {
            return Err(HarnessError::ToolConfirmationConflict(
                "record_note cannot run without a confirmation".into(),
            ));
        }
        Ok(
            json!({"recorded": false, "reason": "this example stores nothing", "note": arguments["note"]}),
        )
    }
}

#[derive(Default)]
struct KindCounter {
    counts: Mutex<BTreeMap<&'static str, u64>>,
}

#[async_trait]
impl EventSink for KindCounter {
    async fn handle(&self, notification: &SinkNotification) -> Result<()> {
        let mut counts = self.counts.lock().expect("counter lock");
        match notification {
            SinkNotification::Event(envelope) => {
                *counts.entry(envelope.event.kind()).or_default() += 1;
            }
            SinkNotification::ResyncRequired(_) => counts.clear(),
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut registry = ExtensionRegistry::default();
    registry.register_image_backend(EXAMPLE, Arc::new(OfflineBackend))?;
    registry.register_llm_provider(EXAMPLE, Arc::new(NoModelProvider))?;
    registry.register_assistant_tool(Arc::new(RecordNoteTool))?;
    registry.register_event_sink("kind-counter", Arc::new(KindCounter::default()))?;

    let backend = registry.image_backend(EXAMPLE).expect("registered backend");
    assert!(
        backend.health().await.is_err(),
        "the example never reports success"
    );
    assert!(registry
        .register_image_backend(EXAMPLE, Arc::new(OfflineBackend))
        .is_err());

    let tool = registry
        .assistant_tool("record_note")
        .expect("registered tool");
    let refused = tool
        .invoke(
            ToolContext {
                conversation_id: "example".into(),
                tool_call_id: "call-1".into(),
                confirmation_id: None,
            },
            json!({"note": "hello"}),
        )
        .await;
    assert!(refused.is_err(), "the tool never runs unconfirmed");

    let bus = EventBus::default();
    let shutdown = CancellationToken::new();
    let tracker = TaskTracker::new();
    registry.forward_event_sinks(&bus, SinkRuntime::default(), &shutdown, &tracker);
    bus.publish(Event::SettingsChanged {
        settings: json!({}),
    });
    shutdown.cancel();
    tracker.close();
    tracker.wait().await;

    println!("extensions registered and checked");
    Ok(())
}
