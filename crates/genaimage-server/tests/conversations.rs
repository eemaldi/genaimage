mod common;

use std::time::Duration;

use common::ApiHarness;
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantStreamEvent, Client, Error},
    AssistantChatContent, AssistantConversationDetailQuery, AssistantConversationListQuery,
    AssistantMessageRole, AssistantMessageStatus, CreateAssistantConversationRequest,
    SendAssistantMessageRequest,
};
use genaimage_core::assistant::NewAssistantConversation;
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::StatusCode;
use serde_json::json;
use tokio::time::{sleep, timeout};

async fn configured(script: FakeLlmScript) -> (ApiHarness, FakeLlmServer) {
    let script = script.with_models_response(json!({
        "object": "list",
        "data": [{
            "id": "assistant",
            "supports_tools": true,
            "supports_vision": false
        }]
    }));
    let llm = FakeLlmServer::start(script).await;
    let base_url = llm.base_url();
    let api = ApiHarness::start_with_config(
        5,
        &[
            ("GENAIMAGE_LLM_URL", &base_url),
            ("GENAIMAGE_LLM_TEXT_MODEL", "assistant"),
        ],
    )
    .await;
    (api, llm)
}

fn client(api: &ApiHarness) -> Client {
    Client::with_base_url(&api.origin).unwrap()
}

fn text_request(text: &str) -> SendAssistantMessageRequest {
    SendAssistantMessageRequest {
        content: vec![AssistantChatContent::Text(text.to_owned())],
    }
}

async fn create(client: &Client, title: &str) -> genaimage_api::AssistantConversationResponse {
    client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: title.to_owned(),
        })
        .await
        .unwrap()
}

fn assert_problem(error: Error, status: StatusCode) -> genaimage_api::ProblemDetails {
    match error {
        Error::Problem(problem) => {
            assert_eq!(problem.status, status.as_u16());
            *problem
        }
        other => panic!("expected problem details, got {other}"),
    }
}

async fn next_started(
    stream: &mut genaimage_api::client::AssistantEventStream,
) -> AssistantMessageStarted {
    match timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        AssistantStreamEvent::MessageStarted(event) => AssistantMessageStarted(event),
        _ => panic!("expected message_started"),
    }
}

struct AssistantMessageStarted(genaimage_api::AssistantMessageStartedEvent);

#[tokio::test]
async fn streams_text_tool_proposals_and_persisted_terminal_over_sse() {
    let (api, llm) = configured(
        FakeLlmScript::default().with_events([
            FakeLlmEvent::text("Hello"),
            FakeLlmEvent::tool_calls([FakeToolCallDelta::new(0)
                .id("call-1")
                .name("review_output")
                .arguments("{\"output_id\":\"out-1\"}")]),
            FakeLlmEvent::finish("tool_calls"),
            FakeLlmEvent::Done,
        ]),
    )
    .await;
    let client = client(&api);
    let conversation = create(&client, "Streaming").await;
    let mut stream = client
        .send_assistant_message(&conversation.id, &text_request("Hello?"))
        .await
        .unwrap();
    let started = next_started(&mut stream).await;
    assert_eq!(started.0.conversation_id, conversation.id);
    assert_eq!(started.0.model, "assistant");
    assert!(!started.0.user_message_id.is_empty());
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::Delta(event) if event.text == "Hello"
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::ToolCall(event)
            if event.tool_call.id == "call-1"
                && event.tool_call.name == "review_output"
                && event.tool_call.arguments["output_id"] == "out-1"
    ));
    let terminal = match stream.next().await.unwrap().unwrap() {
        AssistantStreamEvent::Done(event) => event,
        _ => panic!("expected done"),
    };
    assert_eq!(terminal.status, AssistantMessageStatus::Complete);
    assert_eq!(terminal.model, "assistant");
    assert!(!terminal.message_id.is_empty());
    assert!(stream.next().await.is_none());

    let detail = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[0].role, AssistantMessageRole::User);
    assert_eq!(detail.messages[1].id, terminal.message_id);
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Complete);
    assert_eq!(detail.messages[1].model.as_deref(), Some("assistant"));
    assert_eq!(llm.chat_request_count(), 1);
}

#[tokio::test]
async fn cancel_from_another_request_stops_stream_and_persists_partial_text() {
    let (api, llm) = configured(FakeLlmScript::default().with_events([
        FakeLlmEvent::text("partial"),
        FakeLlmEvent::Delay(Duration::from_secs(10)),
        FakeLlmEvent::finish("stop"),
        FakeLlmEvent::Done,
    ]))
    .await;
    let client = client(&api);
    let conversation = create(&client, "Cancel").await;
    let mut stream = client
        .send_assistant_message(&conversation.id, &text_request("continue"))
        .await
        .unwrap();
    next_started(&mut stream).await;
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::Delta(event) if event.text == "partial"
    ));

    timeout(
        Duration::from_secs(2),
        client.cancel_assistant_conversation(&conversation.id),
    )
    .await
    .expect("separate cancel request should finish promptly")
    .unwrap();
    let terminal = match timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        AssistantStreamEvent::Error(event) => event,
        _ => panic!("expected cancelled error event"),
    };
    assert_eq!(terminal.status, AssistantMessageStatus::Cancelled);
    assert!(!terminal.message_id.is_empty());
    assert!(stream.next().await.is_none());

    let detail = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert_eq!(detail.messages[1].id, terminal.message_id);
    assert!(matches!(
        &detail.messages[1].content,
        genaimage_api::AssistantChatMessage::Assistant { text, .. } if text == "partial"
    ));
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Cancelled);
    assert!(llm.wait_for_stream_disconnect(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn the_role_request_check_rejects_unsupported_images_before_provider_call() {
    let (api, llm) = configured(FakeLlmScript::default()).await;
    let client = client(&api);
    let conversation = create(&client, "No vision").await;
    let error = client
        .send_assistant_message(
            &conversation.id,
            &SendAssistantMessageRequest {
                content: vec![AssistantChatContent::Image {
                    media_type: "image/png".into(),
                    bytes: vec![1, 2, 3],
                }],
            },
        )
        .await
        .err()
        .expect("unsupported image should be rejected");
    let problem = assert_problem(error, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.detail.contains("vision capability"));
    assert_eq!(llm.chat_request_count(), 0);
    let detail = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert!(detail.messages.is_empty());
}

#[tokio::test]
async fn a_second_concurrent_send_returns_conflict() {
    let (api, _llm) = configured(
        FakeLlmScript::default().with_events([FakeLlmEvent::text("partial"), FakeLlmEvent::Hang]),
    )
    .await;
    let client = client(&api);
    let conversation = create(&client, "One at a time").await;
    let mut stream = client
        .send_assistant_message(&conversation.id, &text_request("first"))
        .await
        .unwrap();
    next_started(&mut stream).await;
    let error = client
        .send_assistant_message(&conversation.id, &text_request("second"))
        .await
        .err()
        .expect("second send should fail");
    assert_problem(error, StatusCode::CONFLICT);
    client
        .cancel_assistant_conversation(&conversation.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn not_configured_and_unknown_conversation_are_distinct_problems() {
    let api = ApiHarness::start().await;
    let unavailable_client = client(&api);
    let error = unavailable_client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Unavailable".into(),
        })
        .await
        .unwrap_err();
    let problem = assert_problem(error, StatusCode::SERVICE_UNAVAILABLE);
    assert!(problem.detail.contains("llm.base_url is empty"));
    assert!(problem.recovery.contains("Settings"));
    api.shutdown().await;

    let (api, _llm) = configured(FakeLlmScript::default()).await;
    let client = client(&api);
    let error = client
        .assistant_conversation(
            "00000000-0000-0000-0000-000000000000",
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap_err();
    assert_problem(error, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_removes_a_conversation() {
    let (api, _llm) = configured(FakeLlmScript::default()).await;
    let client = client(&api);
    let conversation = create(&client, "Delete").await;
    client
        .delete_assistant_conversation(&conversation.id)
        .await
        .unwrap();
    let error = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap_err();
    assert_problem(error, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pagination_and_message_limits_are_bounded() {
    let (api, _llm) = configured(FakeLlmScript::default()).await;
    let client = client(&api);
    let mut conversations = Vec::new();
    for title in ["One", "Two", "Three"] {
        conversations.push(create(&client, title).await);
    }
    let first = client
        .assistant_conversations(&AssistantConversationListQuery {
            cursor: None,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(first.conversations.len(), 2);
    assert!(first.next_cursor.is_some());
    let second = client
        .assistant_conversations(&AssistantConversationListQuery {
            cursor: first.next_cursor,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(second.conversations.len(), 1);
    assert!(second.next_cursor.is_none());

    let error = client
        .assistant_conversations(&AssistantConversationListQuery {
            cursor: None,
            limit: Some(101),
        })
        .await
        .unwrap_err();
    assert_problem(error, StatusCode::BAD_REQUEST);
    let error = client
        .assistant_conversation(
            &conversations[0].id,
            &AssistantConversationDetailQuery {
                message_limit: Some(501),
            },
        )
        .await
        .unwrap_err();
    assert_problem(error, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_later_turn_uses_the_current_explicit_model_and_records_it() {
    let (api, llm) = configured(FakeLlmScript::text_chunks(["new model reply"])).await;
    let conversation = api
        .core
        .conversation_store()
        .create(NewAssistantConversation {
            title: "Old model".into(),
            model: "previous-model".into(),
            system_prompt_template: "system".into(),
            system_prompt_sha256: "test-snapshot".into(),
            system_prompt: "Explicit system prompt".into(),
        })
        .await
        .unwrap();
    let client = client(&api);
    let mut stream = client
        .send_assistant_message(&conversation.id, &text_request("Which model?"))
        .await
        .unwrap();
    let started = next_started(&mut stream).await;
    assert_eq!(started.0.model, "assistant");
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::Delta(event) if event.text == "new model reply"
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::Done(event) if event.model == "assistant"
    ));
    let detail = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert_eq!(detail.model, "previous-model");
    assert_eq!(detail.messages[1].model.as_deref(), Some("assistant"));
    assert_eq!(llm.recorded_requests()[0]["model"], "assistant");
}

#[tokio::test]
async fn dropping_the_sse_client_persists_a_cancelled_partial_message() {
    let (api, _llm) = configured(FakeLlmScript::default().with_events([
        FakeLlmEvent::text("disconnect partial"),
        FakeLlmEvent::Delay(Duration::from_secs(10)),
        FakeLlmEvent::finish("stop"),
        FakeLlmEvent::Done,
    ]))
    .await;
    let client = client(&api);
    let conversation = create(&client, "Disconnect").await;
    let mut stream = client
        .send_assistant_message(&conversation.id, &text_request("keep writing"))
        .await
        .unwrap();
    next_started(&mut stream).await;
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        AssistantStreamEvent::Delta(event) if event.text == "disconnect partial"
    ));
    drop(stream);

    let detail = timeout(Duration::from_secs(3), async {
        loop {
            let detail = client
                .assistant_conversation(
                    &conversation.id,
                    &AssistantConversationDetailQuery::default(),
                )
                .await
                .unwrap();
            if detail.messages.len() == 2 {
                return detail;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("client disconnect should persist cancellation");
    assert_eq!(detail.messages[1].status, AssistantMessageStatus::Cancelled);
    assert!(matches!(
        &detail.messages[1].content,
        genaimage_api::AssistantChatMessage::Assistant { text, .. }
            if text == "disconnect partial"
    ));
}
