use std::time::Duration;

use futures_util::StreamExt;
use genaimage_core::{
    models::{LlmModelManager, ModelSource, PullEvent, PullStream},
    HarnessError,
};
use genaimage_llm::{
    OllamaModelManager, OpenAiCompatibleProvider, OpenAiCompatibleProviderOptions,
};
use genaimage_test_support::{
    FakeLlmScript, FakeLlmServer, FakeOllamaScript, FakeOllamaServer, FakePullStep,
};
use tokio::time::timeout;

const SECRET: &str = "sk-ollama-secret-4711";
const BOUND: Duration = Duration::from_secs(10);

fn manager(base_url: &str, api_key: Option<&str>, idle: Duration) -> OllamaModelManager {
    OllamaModelManager::new(
        OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
            base_url: base_url.to_owned(),
            api_key: api_key.map(str::to_owned),
            idle_timeout: idle,
            ..Default::default()
        })
        .unwrap(),
    )
}

async fn script(steps: Vec<FakePullStep>) -> FakeOllamaServer {
    FakeOllamaServer::start(FakeOllamaScript {
        pull: steps,
        ..Default::default()
    })
    .await
}

async fn run_pull(server: &FakeOllamaServer, manager: &OllamaModelManager) -> Vec<PullEvent> {
    timeout(BOUND, async {
        let stream: PullStream = manager.pull("llama3.2:3b").await.unwrap();
        stream.collect::<Vec<_>>().await
    })
    .await
    .unwrap_or_else(|_| panic!("pull timed out after {} requests", server.pull_count()))
}

fn assert_failed_with(events: &[PullEvent], needle: &str) {
    match events.last() {
        Some(PullEvent::Failed { message }) => {
            assert!(message.contains(needle), "{message}");
            assert!(!message.contains(SECRET));
        }
        other => panic!("expected failure containing {needle:?}, got {other:?}"),
    }
    assert!(!events
        .iter()
        .any(|event| matches!(event, PullEvent::Success)));
}

#[tokio::test]
async fn lists_installed_models_through_the_native_api() {
    let server = FakeOllamaServer::start(FakeOllamaScript::default()).await;
    let manager = manager(&server.base_url(), Some(SECRET), Duration::from_secs(5));
    let listing = timeout(BOUND, manager.list_installed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listing.source, ModelSource::Ollama);
    assert!(listing.pull_supported);
    assert_eq!(listing.pull_unsupported_reason, None);
    assert_eq!(listing.models.len(), 1);
    assert_eq!(listing.models[0].name, "llama3.2:3b");
    assert_eq!(listing.models[0].size_bytes, Some(2_019_393_189));
    assert_eq!(listing.models[0].digest.as_deref(), Some("a80c4f17acd5"));
    assert_eq!(server.pull_count(), 0, "listing must never pull");
    let bearer = format!("Bearer {SECRET}");
    assert!(server
        .authorizations()
        .iter()
        .all(|value| value.as_deref() == Some(bearer.as_str())));
    assert!(!format!("{manager:?}").contains(SECRET));
}

#[tokio::test]
async fn falls_back_to_the_openai_listing_and_reports_pull_unsupported() {
    let server = FakeLlmServer::start(FakeLlmScript::default().with_models(["test-model"])).await;
    let manager = manager(&server.base_url(), None, Duration::from_secs(5));
    let listing = timeout(BOUND, manager.list_installed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listing.source, ModelSource::OpenAi);
    assert!(!listing.pull_supported);
    assert!(listing
        .pull_unsupported_reason
        .as_deref()
        .unwrap()
        .contains("llama.cpp"));
    assert_eq!(listing.models[0].name, "test-model");
    assert_eq!(listing.models[0].size_bytes, None);
    let error = match manager.pull("llama3.2:3b").await {
        Err(error) => error,
        Ok(_) => panic!("pull must be unsupported"),
    };
    assert!(
        matches!(error, HarnessError::UnsupportedOperation(_)),
        "{error}"
    );
}

#[tokio::test]
async fn pull_streams_progress_in_order_and_ends_with_success() {
    let server = script(FakePullStep::default_steps()).await;
    let manager = manager(&server.base_url(), None, Duration::from_secs(5));
    let events = run_pull(&server, &manager).await;
    let statuses = events
        .iter()
        .map(|event| match event {
            PullEvent::Progress(progress) => progress.status.clone(),
            PullEvent::Success => "<success>".into(),
            PullEvent::Failed { message } => panic!("{message}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        [
            "pulling manifest",
            "pulling sha256:aaa",
            "pulling sha256:aaa",
            "verifying sha256 digest",
            "<success>"
        ]
    );
    let PullEvent::Progress(layer) = &events[1] else {
        unreachable!()
    };
    assert_eq!(layer.total_bytes, Some(1000));
    assert_eq!(layer.completed_bytes, Some(250));
    assert_eq!(layer.digest.as_deref(), Some("sha256:aaa"));
    assert_eq!(server.pull_requests().len(), 1);
    assert_eq!(server.pull_requests()[0]["model"], "llama3.2:3b");
    assert_eq!(server.pull_requests()[0]["stream"], true);
}

#[tokio::test]
async fn an_error_line_fails_the_pull_and_redacts_the_key() {
    let server = script(vec![
        FakePullStep::progress("pulling manifest"),
        FakePullStep::error(&format!(
            "pull failed for token {SECRET}: file does not exist"
        )),
    ])
    .await;
    let manager = manager(&server.base_url(), Some(SECRET), Duration::from_secs(5));
    let events = run_pull(&server, &manager).await;
    assert_failed_with(&events, "file does not exist");
    let Some(PullEvent::Failed { message }) = events.last() else {
        unreachable!()
    };
    assert!(message.contains("[REDACTED]"));
}

#[tokio::test]
async fn a_stream_without_the_success_terminator_is_a_failure() {
    for steps in [
        vec![FakePullStep::progress("pulling manifest")],
        vec![
            FakePullStep::progress("pulling manifest"),
            FakePullStep::layer("sha256:aaa", 10, 10),
        ],
        vec![],
    ] {
        let server = script(steps).await;
        let manager = manager(&server.base_url(), None, Duration::from_secs(5));
        let events = run_pull(&server, &manager).await;
        assert_failed_with(&events, "without Ollama reporting success");
    }
}

#[tokio::test]
async fn success_without_a_trailing_newline_still_counts() {
    let server = script(vec![FakePullStep::Raw(r#"{"status":"success"}"#.into())]).await;
    let manager = manager(&server.base_url(), None, Duration::from_secs(5));
    assert_eq!(run_pull(&server, &manager).await, [PullEvent::Success]);
}

#[tokio::test]
async fn an_invalid_line_fails_the_pull() {
    for line in ["not json\n", "{\"digest\":\"x\"}\n", "{\"status\":1}\n"] {
        let server = script(vec![
            FakePullStep::Raw(line.into()),
            FakePullStep::success(),
        ])
        .await;
        let manager = manager(&server.base_url(), None, Duration::from_secs(5));
        let events = run_pull(&server, &manager).await;
        assert!(
            matches!(events.last(), Some(PullEvent::Failed { .. })),
            "{line}"
        );
        assert_eq!(events.len(), 1, "no event after a failure");
    }
}

#[tokio::test]
async fn an_oversized_line_fails_the_pull() {
    for raw in [format!("{}\n", "x".repeat(200_000)), "y".repeat(200_000)] {
        let server = script(vec![
            FakePullStep::progress("pulling manifest"),
            FakePullStep::Raw(raw),
            FakePullStep::success(),
        ])
        .await;
        let manager = manager(&server.base_url(), None, Duration::from_secs(5));
        let events = run_pull(&server, &manager).await;
        assert_failed_with(&events, "longer than");
    }
}

#[tokio::test]
async fn an_idle_stream_fails_after_the_idle_timeout() {
    let server = script(vec![
        FakePullStep::progress("pulling manifest"),
        FakePullStep::Hang,
    ])
    .await;
    let manager = manager(&server.base_url(), None, Duration::from_millis(50))
        .with_pull_idle_timeout(Duration::from_millis(200));
    let events = run_pull(&server, &manager).await;
    assert_eq!(events.len(), 2);
    assert_failed_with(&events, "no data");
}

#[tokio::test]
async fn a_slow_but_active_pull_stops_at_the_overall_limit() {
    let mut steps = Vec::new();
    for _ in 0..20 {
        steps.push(FakePullStep::progress("pulling"));
        steps.push(FakePullStep::Delay(Duration::from_millis(100)));
    }
    let server = script(steps).await;
    let manager = manager(&server.base_url(), None, Duration::from_secs(5))
        .with_pull_limits(64 * 1024, Duration::from_millis(350));
    let events = run_pull(&server, &manager).await;
    assert_failed_with(&events, "limit");
}

#[tokio::test]
async fn an_http_error_before_streaming_is_an_error_with_the_key_redacted() {
    let server = FakeOllamaServer::start(FakeOllamaScript {
        pull_status: Some((
            reqwest::StatusCode::NOT_FOUND,
            format!("pull model manifest: file does not exist ({SECRET})"),
        )),
        ..Default::default()
    })
    .await;
    let manager = manager(&server.base_url(), Some(SECRET), Duration::from_secs(5));
    let error = match manager.pull("nope").await {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected an error"),
    };
    assert!(
        error.contains("404") && error.contains("file does not exist"),
        "{error}"
    );
    assert!(!error.contains(SECRET));
}

#[tokio::test]
async fn an_unreachable_endpoint_is_an_error_not_unsupported() {
    let manager = manager("http://127.0.0.1:1/v1", None, Duration::from_secs(1));
    let error = timeout(BOUND, manager.list_installed())
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        !matches!(error, HarnessError::UnsupportedOperation(_)),
        "{error}"
    );
}
