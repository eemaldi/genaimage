use std::time::Duration;

use futures_util::StreamExt;
use genaimage_core::{
    extensions::{ChatContent, ChatDelta, ChatMessage, ChatRequest, ToolCall, ToolDefinition},
    HarnessError, LlmProvider,
};
use genaimage_llm::{OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use serde_json::json;

fn request() -> ChatRequest {
    ChatRequest {
        model: "explicit-model".to_owned(),
        messages: vec![ChatMessage::User(vec![ChatContent::Text(
            "hello".to_owned(),
        )])],
        tools: Vec::new(),
    }
}

fn provider(server: &FakeLlmServer) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
        base_url: server.base_url(),
        first_byte_timeout: Duration::from_millis(250),
        idle_timeout: Duration::from_millis(100),
        connect_timeout: Duration::from_millis(100),
        ..Default::default()
    })
    .expect("provider")
}

async fn collect_stream(
    provider: &OpenAiCompatibleProvider,
    request: ChatRequest,
) -> Vec<ChatDelta> {
    let mut stream = provider.chat_stream(request).await.expect("chat stream");
    let mut output = Vec::new();
    while let Some(delta) = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("stream deadline")
    {
        output.push(delta.expect("stream item"));
    }
    output
}

#[tokio::test]
async fn streams_text_deltas_without_rebuilding_the_request() {
    let server = FakeLlmServer::start(FakeLlmScript::text_chunks(["Hello", " world"])).await;
    let provider = provider(&server);

    let output = collect_stream(&provider, request()).await;

    assert!(
        matches!(&output[..], [ChatDelta::Text(a), ChatDelta::Text(b)] if a == "Hello" && b == " world")
    );
    assert_eq!(server.recorded_requests()[0]["model"], "explicit-model");
    assert_eq!(server.recorded_requests()[0]["stream"], true);
}

#[tokio::test]
async fn assembles_multiple_fragmented_tool_calls_in_index_order() {
    let script = FakeLlmScript::default().with_events([
        FakeLlmEvent::tool_calls([
            FakeToolCallDelta::new(1)
                .id("call-weather")
                .name("weather")
                .arguments("{\"city\":"),
            FakeToolCallDelta::new(0)
                .id("call-time")
                .name("time_")
                .arguments("{\"zone\""),
        ]),
        FakeLlmEvent::tool_calls([
            FakeToolCallDelta::new(0).name("now").arguments(":\"UTC\"}"),
            FakeToolCallDelta::new(1).arguments("\"Paris\"}"),
        ]),
        FakeLlmEvent::finish("tool_calls"),
        FakeLlmEvent::Done,
    ]);
    let server = FakeLlmServer::start(script).await;
    let provider = provider(&server);
    let mut chat = request();
    chat.tools = vec![ToolDefinition {
        name: "weather".to_owned(),
        description: "Get weather".to_owned(),
        input_schema: json!({ "type": "object" }),
    }];

    let output = collect_stream(&provider, chat).await;

    assert_eq!(output.len(), 2);
    assert!(
        matches!(&output[0], ChatDelta::ToolCall(call) if call.id == "call-time" && call.name == "time_now" && call.arguments == json!({"zone":"UTC"}))
    );
    assert!(
        matches!(&output[1], ChatDelta::ToolCall(call) if call.id == "call-weather" && call.name == "weather" && call.arguments == json!({"city":"Paris"}))
    );
}

#[tokio::test]
async fn vision_tools_and_tool_results_use_openai_wire_shapes() {
    let server = FakeLlmServer::start(FakeLlmScript::default()).await;
    let provider = provider(&server);
    let chat = ChatRequest {
        model: "vision-model".to_owned(),
        messages: vec![
            ChatMessage::User(vec![
                ChatContent::Text("inspect".to_owned()),
                ChatContent::Image {
                    media_type: "image/png".to_owned(),
                    bytes: vec![1, 2, 3],
                },
            ]),
            ChatMessage::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "inspect".to_owned(),
                    arguments: json!({"detail": "high"}),
                }],
            },
            ChatMessage::Tool {
                call_id: "call-1".to_owned(),
                result: json!({"ok": true}),
            },
        ],
        tools: vec![ToolDefinition {
            name: "inspect".to_owned(),
            description: "Inspect image".to_owned(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }],
    };

    collect_stream(&provider, chat).await;

    let body = &server.recorded_requests()[0];
    assert_eq!(
        body.pointer("/messages/0/content/1"),
        Some(&json!({
            "type": "image_url",
            "image_url": { "url": "data:image/png;base64,AQID" }
        }))
    );
    assert_eq!(body.pointer("/tools/0/type"), Some(&json!("function")));
    assert_eq!(
        body.pointer("/messages/2"),
        Some(&json!({
            "role": "tool",
            "tool_call_id": "call-1",
            "content": "{\"ok\":true}"
        }))
    );
}

#[tokio::test]
async fn lists_explicit_model_ids() {
    let server =
        FakeLlmServer::start(FakeLlmScript::default().with_models(["first-model", "second-model"]))
            .await;
    let models = provider(&server).list_models().await.expect("models");
    assert_eq!(
        models.into_iter().map(|model| model.id).collect::<Vec<_>>(),
        ["first-model", "second-model"]
    );
}

#[tokio::test]
async fn maps_client_and_server_errors_with_server_message() {
    for (status, expected) in [(400, "bad request detail"), (503, "backend overloaded")] {
        let server =
            FakeLlmServer::start(FakeLlmScript::default().with_chat_error(status, expected)).await;
        let error = provider(&server)
            .chat_stream(request())
            .await
            .err()
            .expect("chat error");
        let text = error.to_string();
        assert!(text.contains(&status.to_string()), "{text}");
        assert!(text.contains(expected), "{text}");
    }
}

#[tokio::test]
async fn reports_unsupported_capabilities_without_fallback() {
    let server = FakeLlmServer::start(
        FakeLlmScript::default().with_chat_error(400, "tools are unsupported for this model"),
    )
    .await;
    let mut chat = request();
    chat.tools.push(ToolDefinition {
        name: "inspect".to_owned(),
        description: "Inspect".to_owned(),
        input_schema: json!({ "type": "object" }),
    });
    let error = provider(&server)
        .chat_stream(chat)
        .await
        .err()
        .expect("chat error");
    assert!(
        matches!(error, HarnessError::Configuration(message) if message.contains("explicit-model") && message.contains("tool-calling"))
    );
    assert_eq!(server.chat_request_count(), 1);
}

#[tokio::test]
async fn reports_unsupported_vision_without_retrying_as_text() {
    let server = FakeLlmServer::start(
        FakeLlmScript::default().with_chat_error(400, "image input is not supported"),
    )
    .await;
    let mut chat = request();
    if let ChatMessage::User(content) = &mut chat.messages[0] {
        content.push(ChatContent::Image {
            media_type: "image/jpeg".to_owned(),
            bytes: vec![4, 5, 6],
        });
    }
    let error = provider(&server)
        .chat_stream(chat)
        .await
        .err()
        .expect("vision error");
    assert!(
        matches!(error, HarnessError::Configuration(message) if message.contains("explicit-model") && message.contains("vision/image"))
    );
    assert_eq!(server.chat_request_count(), 1);
}

#[tokio::test]
async fn reports_connection_refusal_as_unavailable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
        base_url: format!("http://{address}/v1"),
        ..Default::default()
    })
    .expect("provider");
    let error = provider
        .chat_stream(request())
        .await
        .err()
        .expect("chat error");
    assert!(error.to_string().contains("unavailable"), "{error}");
}

#[tokio::test]
async fn reports_first_byte_and_idle_timeouts() {
    let slow_headers = FakeLlmServer::start(
        FakeLlmScript::default().with_response_delay(Duration::from_millis(500)),
    )
    .await;
    let error = provider(&slow_headers)
        .chat_stream(request())
        .await
        .err()
        .expect("chat error");
    assert!(error.to_string().contains("timed out"), "{error}");

    let hanging =
        FakeLlmServer::start(FakeLlmScript::default().with_events([FakeLlmEvent::Hang])).await;
    let mut stream = provider(&hanging)
        .chat_stream(request())
        .await
        .expect("stream");
    let error = stream.next().await.expect("item").unwrap_err();
    assert!(error.to_string().contains("idle"), "{error}");
}

#[tokio::test]
async fn rejects_invalid_tool_argument_json() {
    let server = FakeLlmServer::start(
        FakeLlmScript::default().with_events([
            FakeLlmEvent::tool_calls([FakeToolCallDelta::new(0)
                .id("call-1")
                .name("broken")
                .arguments("{not-json")]),
            FakeLlmEvent::finish("tool_calls"),
        ]),
    )
    .await;
    let mut stream = provider(&server)
        .chat_stream(request())
        .await
        .expect("stream");
    let error = stream.next().await.expect("item").unwrap_err();
    assert!(error.to_string().contains("invalid JSON arguments"));
}

#[tokio::test]
async fn reports_malformed_sse_data() {
    let server = FakeLlmServer::start(
        FakeLlmScript::default()
            .with_events([FakeLlmEvent::RawSse("data: {not-json\n\n".to_owned())]),
    )
    .await;
    let mut stream = provider(&server)
        .chat_stream(request())
        .await
        .expect("stream");
    let error = stream.next().await.expect("item").unwrap_err();
    assert!(error.to_string().contains("invalid JSON"), "{error}");
}

#[tokio::test]
async fn dropping_stream_cancels_the_http_body_promptly() {
    let server =
        FakeLlmServer::start(FakeLlmScript::default().with_events([FakeLlmEvent::Hang])).await;
    let stream = provider(&server)
        .chat_stream(request())
        .await
        .expect("stream");
    assert!(
        server
            .wait_for_chat_requests(1, Duration::from_secs(1))
            .await
    );
    drop(stream);
    assert!(
        server
            .wait_for_stream_disconnect(Duration::from_secs(1))
            .await
    );
}
