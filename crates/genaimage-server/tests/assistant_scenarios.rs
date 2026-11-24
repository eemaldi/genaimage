mod common;

use std::{collections::BTreeSet, sync::atomic::Ordering, time::Duration};

use common::{install_workflow, ApiHarness};
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantEventStream, AssistantStreamEvent, Client, Error},
    AssistantChatContent, AssistantConfirmationDecisionKind, AssistantConfirmationPreviewRequest,
    AssistantConfirmationRequest, AssistantConversationDetailQuery, AssistantMessageRole,
    AssistantMessageStatus, AssistantToolAuditQuery, AssistantToolConfirmationStatus,
    CreateAssistantConversationRequest, SendAssistantMessageRequest,
};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::time::timeout;
use url::{Host, Url};

const MODEL: &str = "scenario-model";
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalKind {
    AwaitingConfirmation,
    Done,
    Error,
}

#[derive(Clone, Debug)]
struct ExpectedStream {
    events: &'static [&'static str],
    terminal: TerminalKind,
    status: AssistantMessageStatus,
}

impl ExpectedStream {
    fn awaiting(events: &'static [&'static str]) -> Self {
        Self {
            events,
            terminal: TerminalKind::AwaitingConfirmation,
            status: AssistantMessageStatus::AwaitingConfirmation,
        }
    }

    fn done(events: &'static [&'static str]) -> Self {
        Self {
            events,
            terminal: TerminalKind::Done,
            status: AssistantMessageStatus::Complete,
        }
    }

    fn error(events: &'static [&'static str], status: AssistantMessageStatus) -> Self {
        Self {
            events,
            terminal: TerminalKind::Error,
            status,
        }
    }
}

#[derive(Clone, Debug)]
enum ModelTurn {
    Text(&'static str),
    Tool {
        id: &'static str,
        name: &'static str,
        arguments: Arguments,
    },
}

#[derive(Clone, Debug)]
enum Arguments {
    Exact(Value),
    LastRun,
}

impl Arguments {
    fn resolve(&self, harness: &ScenarioHarness) -> Value {
        match self {
            Self::Exact(value) => value.clone(),
            Self::LastRun => {
                let runs = harness.api.core.list_runs(10).unwrap();
                let run = runs.last().expect("scenario must have created one run");
                json!({"run_id": run.id})
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    Send {
        text: &'static str,
        turns: Vec<ModelTurn>,
        outcome: SendOutcome,
    },
    Preview {
        arguments: Value,
    },
    Confirm {
        turns: Vec<ModelTurn>,
        outcome: ExpectedStream,
    },
    Edit {
        arguments: Value,
        turns: Vec<ModelTurn>,
        outcome: ExpectedStream,
    },
    Reject {
        turns: Vec<ModelTurn>,
        outcome: ExpectedStream,
    },
    StaleEdit {
        arguments: Value,
        status: StatusCode,
    },
    ConfirmAgain {
        status: StatusCode,
    },
    CompleteBackend,
    Cancel {
        text: &'static str,
        partial: &'static str,
        outcome: ExpectedStream,
    },
}

#[derive(Clone, Debug)]
enum SendOutcome {
    Stream(ExpectedStream),
    Problem(StatusCode, &'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedMessage {
    role: AssistantMessageRole,
    status: AssistantMessageStatus,
}

impl ExpectedMessage {
    fn user() -> Self {
        Self {
            role: AssistantMessageRole::User,
            status: AssistantMessageStatus::Complete,
        }
    }

    fn assistant() -> Self {
        Self {
            role: AssistantMessageRole::Assistant,
            status: AssistantMessageStatus::Complete,
        }
    }

    fn assistant_status(status: AssistantMessageStatus) -> Self {
        Self {
            role: AssistantMessageRole::Assistant,
            status,
        }
    }

    fn tool() -> Self {
        Self {
            role: AssistantMessageRole::Tool,
            status: AssistantMessageStatus::Complete,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedAudit {
    tool: &'static str,
    decision: &'static str,
    invocation_ok: Option<bool>,
    confirmation: Option<&'static str>,
}

impl ExpectedAudit {
    fn automatic(tool: &'static str, invocation_ok: bool) -> Self {
        Self {
            tool,
            decision: "automatic",
            invocation_ok: Some(invocation_ok),
            confirmation: None,
        }
    }

    fn decision(
        tool: &'static str,
        decision: &'static str,
        invocation_ok: Option<bool>,
        confirmation: &'static str,
    ) -> Self {
        Self {
            tool,
            decision,
            invocation_ok,
            confirmation: Some(confirmation),
        }
    }

    fn invalid(tool: &'static str) -> Self {
        Self {
            tool,
            decision: "not_invoked",
            invocation_ok: Some(false),
            confirmation: None,
        }
    }
}

#[derive(Clone, Debug)]
struct ExpectedFinal {
    messages: Vec<ExpectedMessage>,
    audits: Vec<ExpectedAudit>,
    prompt_calls: usize,
    runs: usize,
}

#[derive(Clone, Debug)]
struct Scenario {
    name: &'static str,
    supports_tools: bool,
    steps: Vec<Step>,
    expected: ExpectedFinal,
}

#[derive(Clone, Debug)]
struct PendingConfirmation {
    id: String,
    token: String,
}

#[derive(Debug)]
struct TerminalObservation {
    kind: TerminalKind,
    status: AssistantMessageStatus,
    message_id: Option<String>,
}

#[derive(Debug)]
struct Observation {
    events: Vec<&'static str>,
    confirmation: Option<PendingConfirmation>,
    terminal: Option<TerminalObservation>,
}

struct ScenarioHarness {
    name: &'static str,
    api: ApiHarness,
    llm: FakeLlmServer,
    client: Client,
    conversation_id: String,
    revision: String,
    supports_tools: bool,
    pending: Option<PendingConfirmation>,
    last_confirmation: Option<PendingConfirmation>,
    latest_preview_token: Option<String>,
}

impl ScenarioHarness {
    async fn start(name: &'static str, supports_tools: bool) -> Self {
        let script = FakeLlmScript::default().with_models_response(models_response(supports_tools));
        let llm = FakeLlmServer::start(script).await;
        let llm_url = llm.base_url();
        let api = ApiHarness::start_with_builtin_assistant_tools(
            5,
            &[
                ("GENAIMAGE_LLM_URL", llm_url.as_str()),
                ("GENAIMAGE_LLM_TEXT_MODEL", MODEL),
            ],
        )
        .await;
        install_workflow(&api);
        let revision = api
            .core
            .load_workflow_by_id("contract-workflow")
            .unwrap()
            .unwrap()
            .revision();
        let client = Client::with_base_url(&api.origin).unwrap();
        let conversation = client
            .create_assistant_conversation(&CreateAssistantConversationRequest {
                title: name.into(),
            })
            .await
            .unwrap();
        Self {
            name,
            api,
            llm,
            client,
            conversation_id: conversation.id,
            revision,
            supports_tools,
            pending: None,
            last_confirmation: None,
            latest_preview_token: None,
        }
    }

    fn run_arguments(&self, seed: u64, steps: u64) -> Value {
        json!({
            "workflow_id": "contract-workflow",
            "workflow_revision": self.revision,
            "preset": "fast",
            "parameters": {"seed": seed, "steps": steps}
        })
    }

    fn model_scripts(&self, turns: &[ModelTurn]) -> Vec<FakeLlmScript> {
        turns
            .iter()
            .map(|turn| match turn {
                ModelTurn::Text(text) => FakeLlmScript::text_chunks([*text])
                    .with_models_response(models_response(self.supports_tools)),
                ModelTurn::Tool {
                    id,
                    name,
                    arguments,
                } => FakeLlmScript::default()
                    .with_events([
                        FakeLlmEvent::tool_calls([FakeToolCallDelta::new(0)
                            .id(*id)
                            .name(*name)
                            .arguments(serde_json::to_string(&arguments.resolve(self)).unwrap())]),
                        FakeLlmEvent::finish("tool_calls"),
                        FakeLlmEvent::Done,
                    ])
                    .with_models_response(models_response(self.supports_tools)),
            })
            .collect()
    }

    fn set_scripts(&self, turns: &[ModelTurn]) {
        if !turns.is_empty() {
            self.llm.set_scripts(self.model_scripts(turns));
        }
    }

    async fn execute(&mut self, scenario: &Scenario) {
        assert_eq!(scenario.name, self.name);
        assert_eq!(scenario.supports_tools, self.supports_tools);
        for step in &scenario.steps {
            match step {
                Step::Send {
                    text,
                    turns,
                    outcome,
                } => self.send(text, turns, outcome).await,
                Step::Preview { arguments } => self.preview(arguments.clone()).await,
                Step::Confirm { turns, outcome } => self.confirm(turns, outcome).await,
                Step::Edit {
                    arguments,
                    turns,
                    outcome,
                } => self.edit(arguments.clone(), turns, outcome).await,
                Step::Reject { turns, outcome } => self.reject(turns, outcome).await,
                Step::StaleEdit { arguments, status } => {
                    self.stale_edit(arguments.clone(), *status).await;
                }
                Step::ConfirmAgain { status } => self.confirm_again(*status).await,
                Step::CompleteBackend => {
                    self.api.fake.complete.store(true, Ordering::SeqCst);
                }
                Step::Cancel {
                    text,
                    partial,
                    outcome,
                } => self.cancel(text, partial, outcome).await,
            }
            self.assert_no_unapproved_submission_for_pending();
        }
        self.assert_final(&scenario.expected).await;
    }

    async fn send(&mut self, text: &str, turns: &[ModelTurn], outcome: &SendOutcome) {
        self.set_scripts(turns);
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let result = self
            .client
            .send_assistant_message(&self.conversation_id, &message(text))
            .await;
        match (result, outcome) {
            (Ok(stream), SendOutcome::Stream(expected)) => {
                let observation = observe(stream).await;
                expected.assert(&observation);
                if let Some(pending) = observation.confirmation {
                    self.latest_preview_token = Some(pending.token.clone());
                    self.last_confirmation = Some(pending.clone());
                    self.pending = Some(pending);
                }
            }
            (Err(error), SendOutcome::Problem(status, detail)) => {
                assert_problem_detail(error, *status, detail);
            }
            (Ok(_), SendOutcome::Problem(status, _)) => {
                panic!("expected HTTP {status} before an SSE stream was opened")
            }
            (Err(error), SendOutcome::Stream(expected)) => {
                panic!("expected SSE {:?}, got {error}", expected.events)
            }
        }
        assert_eq!(
            self.api.fake.prompt_calls.load(Ordering::SeqCst),
            before,
            "user messages must never submit a run in this scenario"
        );
    }

    async fn preview(&mut self, arguments: Value) {
        let pending = self.pending.as_ref().expect("pending confirmation");
        assert_eq!(self.api.fake.prompt_calls.load(Ordering::SeqCst), 0);
        let preview = self
            .client
            .preview_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationPreviewRequest { arguments },
            )
            .await
            .unwrap();
        assert!(preview.preview.is_some());
        self.latest_preview_token = Some(preview.preview_token);
    }

    async fn confirm(&mut self, turns: &[ModelTurn], expected: &ExpectedStream) {
        self.set_scripts(turns);
        let pending = self.pending.clone().expect("pending confirmation");
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let stream = self
            .client
            .decide_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Confirm,
                    arguments: None,
                    reason: None,
                    preview_token: Some(pending.token),
                },
            )
            .await
            .unwrap();
        expected.assert(&observe(stream).await);
        self.pending = None;
        assert_eq!(
            self.api.fake.prompt_calls.load(Ordering::SeqCst),
            before + 1
        );
    }

    async fn edit(&mut self, arguments: Value, turns: &[ModelTurn], expected: &ExpectedStream) {
        self.set_scripts(turns);
        let pending = self.pending.clone().expect("pending confirmation");
        let token = self
            .latest_preview_token
            .clone()
            .expect("fresh preview token");
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let stream = self
            .client
            .decide_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Edit,
                    arguments: Some(arguments),
                    reason: None,
                    preview_token: Some(token),
                },
            )
            .await
            .unwrap();
        expected.assert(&observe(stream).await);
        self.pending = None;
        assert_eq!(
            self.api.fake.prompt_calls.load(Ordering::SeqCst),
            before + 1
        );
    }

    async fn reject(&mut self, turns: &[ModelTurn], expected: &ExpectedStream) {
        self.set_scripts(turns);
        let pending = self.pending.clone().expect("pending confirmation");
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let stream = self
            .client
            .decide_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Reject,
                    arguments: None,
                    reason: Some("declined by scenario".into()),
                    preview_token: None,
                },
            )
            .await
            .unwrap();
        expected.assert(&observe(stream).await);
        self.pending = None;
        assert_eq!(self.api.fake.prompt_calls.load(Ordering::SeqCst), before);
    }

    async fn stale_edit(&mut self, arguments: Value, status: StatusCode) {
        let pending = self.pending.clone().expect("pending confirmation");
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let result = self
            .client
            .decide_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Edit,
                    arguments: Some(arguments),
                    reason: None,
                    preview_token: Some(pending.token),
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("stale edit unexpectedly opened an SSE stream"),
            Err(error) => error,
        };
        assert_problem_status(error, status);
        assert_eq!(self.api.fake.prompt_calls.load(Ordering::SeqCst), before);
        assert!(
            self.pending.is_some(),
            "the stale decision must remain pending"
        );
    }

    async fn confirm_again(&mut self, status: StatusCode) {
        let pending = self.last_confirmation();
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let result = self
            .client
            .decide_assistant_confirmation(
                &self.conversation_id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Confirm,
                    arguments: None,
                    reason: None,
                    preview_token: Some(pending.token),
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("double confirmation unexpectedly opened an SSE stream"),
            Err(error) => error,
        };
        assert_problem_status(error, status);
        assert_eq!(self.api.fake.prompt_calls.load(Ordering::SeqCst), before);
    }

    fn last_confirmation(&self) -> PendingConfirmation {
        self.last_confirmation
            .clone()
            .expect("a confirmation must have been observed")
    }

    async fn cancel(&mut self, text: &str, partial: &str, expected: &ExpectedStream) {
        self.llm.set_script(
            FakeLlmScript::default()
                .with_events([
                    FakeLlmEvent::text(partial),
                    FakeLlmEvent::Delay(Duration::from_secs(10)),
                    FakeLlmEvent::finish("stop"),
                    FakeLlmEvent::Done,
                ])
                .with_models_response(models_response(true)),
        );
        let before = self.api.fake.prompt_calls.load(Ordering::SeqCst);
        let mut stream = self
            .client
            .send_assistant_message(&self.conversation_id, &message(text))
            .await
            .unwrap();
        let mut observation = Observation {
            events: Vec::new(),
            confirmation: None,
            terminal: None,
        };
        record_event(
            &mut observation,
            timeout(STREAM_TIMEOUT, stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
        record_event(
            &mut observation,
            timeout(STREAM_TIMEOUT, stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
        self.client
            .cancel_assistant_conversation(&self.conversation_id)
            .await
            .unwrap();
        finish_observation(&mut stream, &mut observation).await;
        expected.assert(&observation);
        assert!(self.llm.wait_for_stream_disconnect(STREAM_TIMEOUT).await);
        assert_eq!(self.api.fake.prompt_calls.load(Ordering::SeqCst), before);
    }

    fn assert_no_unapproved_submission_for_pending(&self) {
        if self.pending.is_some() {
            assert_eq!(
                self.api.fake.prompt_calls.load(Ordering::SeqCst),
                0,
                "a pending confirmation must not have submitted to ComfyUI"
            );
        }
    }

    async fn assert_final(&self, expected: &ExpectedFinal) {
        let detail = self
            .client
            .assistant_conversation(
                &self.conversation_id,
                &AssistantConversationDetailQuery::default(),
            )
            .await
            .unwrap();
        let actual_messages = detail
            .messages
            .iter()
            .map(|message| ExpectedMessage {
                role: message.role,
                status: message.status,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual_messages, expected.messages,
            "{} persisted message sequence",
            self.name
        );
        let sequence = detail
            .messages
            .iter()
            .map(|message| message.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequence, (1..=sequence.len() as i64).collect::<Vec<_>>());
        assert!(detail.messages.iter().all(
            |message| !message.id.is_empty() && message.conversation_id == self.conversation_id
        ));

        let mut audit = self
            .client
            .assistant_tool_calls(&self.conversation_id, &AssistantToolAuditQuery::default())
            .await
            .unwrap()
            .tool_calls;
        audit.reverse();
        let actual_audit = audit
            .iter()
            .map(|row| ExpectedAudit {
                tool: match row.tool.as_str() {
                    "list_workflows" => "list_workflows",
                    "inspect_workflow" => "inspect_workflow",
                    "propose_run" => "propose_run",
                    "submit_run" => "submit_run",
                    "run_status" => "run_status",
                    other => panic!("unexpected audit tool {other}"),
                },
                decision: match row.decision.as_str() {
                    "automatic" => "automatic",
                    "not_invoked" => "not_invoked",
                    "pending" => "pending",
                    "rejected" => "rejected",
                    "invocation_started" => "invocation_started",
                    "invocation_succeeded" => "invocation_succeeded",
                    other => panic!("unexpected audit decision {other}"),
                },
                invocation_ok: row.invocation_ok,
                confirmation: row
                    .confirmation_status
                    .as_deref()
                    .map(|status| match status {
                        "pending" => "pending",
                        "rejected" => "rejected",
                        "succeeded" => "succeeded",
                        other => panic!("unexpected confirmation status {other}"),
                    }),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_audit, expected.audits, "{} audit rows", self.name);
        assert_eq!(
            self.api.fake.prompt_calls.load(Ordering::SeqCst),
            expected.prompt_calls,
            "{} ComfyUI submissions",
            self.name
        );
        assert_eq!(
            self.api.core.list_runs(100).unwrap().len(),
            expected.runs,
            "{} persisted runs",
            self.name
        );
        let ids = audit.iter().map(|row| row.id).collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), audit.len(), "audit IDs must be unique");
    }
}

impl ExpectedMessage {
    fn sequence(turns: usize, terminal: AssistantMessageStatus) -> Vec<Self> {
        let mut messages = Vec::with_capacity(turns * 2 + 2);
        messages.push(Self::user());
        for _ in 0..turns {
            messages.push(Self::assistant());
            messages.push(Self::tool());
        }
        messages.push(Self::assistant_status(terminal));
        messages
    }
}

impl ExpectedStream {
    fn assert(&self, observation: &Observation) {
        assert_eq!(observation.events, self.events);
        let terminal = observation.terminal.as_ref().expect("terminal SSE event");
        assert_eq!(terminal.kind, self.terminal);
        assert_eq!(terminal.status, self.status);
        assert!(terminal
            .message_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()));
    }
}

async fn observe(stream: AssistantEventStream) -> Observation {
    observe_with_timeout(stream, STREAM_TIMEOUT).await
}

async fn observe_with_timeout(stream: AssistantEventStream, deadline: Duration) -> Observation {
    let mut stream = stream;
    let mut observation = Observation {
        events: Vec::new(),
        confirmation: None,
        terminal: None,
    };
    finish_observation_with_timeout(&mut stream, &mut observation, deadline).await;
    observation
}

async fn finish_observation(stream: &mut AssistantEventStream, observation: &mut Observation) {
    finish_observation_with_timeout(stream, observation, STREAM_TIMEOUT).await;
}

async fn finish_observation_with_timeout(
    stream: &mut AssistantEventStream,
    observation: &mut Observation,
    deadline: Duration,
) {
    timeout(deadline, async {
        while let Some(event) = stream.next().await {
            record_event(observation, event.unwrap());
            if observation.terminal.is_some() {
                assert!(
                    stream.next().await.is_none(),
                    "terminal event must end the stream"
                );
                return;
            }
        }
        panic!("SSE stream ended without a terminal event");
    })
    .await
    .expect("assistant stream did not terminate promptly");
}

fn record_event(observation: &mut Observation, event: AssistantStreamEvent) {
    match event {
        AssistantStreamEvent::MessageStarted(_) => observation.events.push("message_started"),
        AssistantStreamEvent::Delta(_) => observation.events.push("delta"),
        AssistantStreamEvent::ToolCall(_) => observation.events.push("tool_call"),
        AssistantStreamEvent::ToolResult(_) => observation.events.push("tool_result"),
        AssistantStreamEvent::ConfirmationRequired(event) => {
            observation.events.push("confirmation_required");
            assert_eq!(event.status, AssistantToolConfirmationStatus::Pending);
            observation.confirmation = Some(PendingConfirmation {
                id: event.confirmation_id,
                token: event.preview_token,
            });
        }
        AssistantStreamEvent::AwaitingConfirmation(event) => {
            observation.events.push("awaiting_confirmation");
            assert_eq!(event.status, AssistantMessageStatus::AwaitingConfirmation);
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::AwaitingConfirmation,
                status: event.status,
                message_id: Some(event.message_id),
            });
        }
        AssistantStreamEvent::Done(event) => {
            observation.events.push("done");
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::Done,
                status: event.status,
                message_id: event.message_id,
            });
        }
        AssistantStreamEvent::Error(event) => {
            observation.events.push("error");
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::Error,
                status: event.status,
                message_id: event.message_id,
            });
        }
    }
}

fn models_response(supports_tools: bool) -> Value {
    json!({
        "object": "list",
        "data": [{
            "id": MODEL,
            "supports_tools": supports_tools,
            "supports_vision": false
        }]
    })
}

fn message(text: &str) -> SendAssistantMessageRequest {
    SendAssistantMessageRequest {
        content: vec![AssistantChatContent::Text(text.into())],
    }
}

fn assert_problem_status(error: Error, status: StatusCode) {
    assert_problem_detail(error, status, "");
}

fn assert_problem_detail(error: Error, status: StatusCode, detail: &str) {
    match error {
        Error::Problem(problem) => {
            assert_eq!(problem.status, status.as_u16(), "{problem:?}");
            assert!(problem.detail.contains(detail), "{}", problem.detail);
        }
        other => panic!("expected HTTP problem {status}, got {other}"),
    }
}

fn confirmed_messages(turns: usize) -> Vec<ExpectedMessage> {
    ExpectedMessage::sequence(turns, AssistantMessageStatus::Complete)
}

#[tokio::test]
async fn multi_step_tools_require_confirmation_then_check_run_status() {
    let mut harness = ScenarioHarness::start("multi-step tool use", true).await;
    let arguments = harness.run_arguments(17, 6);
    let scenario = Scenario {
        name: "multi-step tool use",
        supports_tools: true,
        steps: vec![
            Step::Send {
                text: "choose and prepare the workflow",
                turns: vec![
                    ModelTurn::Tool {
                        id: "list",
                        name: "list_workflows",
                        arguments: Arguments::Exact(json!({})),
                    },
                    ModelTurn::Tool {
                        id: "inspect",
                        name: "inspect_workflow",
                        arguments: Arguments::Exact(json!({"workflow_id": "contract-workflow"})),
                    },
                    ModelTurn::Tool {
                        id: "propose",
                        name: "propose_run",
                        arguments: Arguments::Exact(arguments.clone()),
                    },
                    ModelTurn::Tool {
                        id: "submit",
                        name: "submit_run",
                        arguments: Arguments::Exact(arguments),
                    },
                ],
                outcome: SendOutcome::Stream(ExpectedStream::awaiting(&[
                    "message_started",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "confirmation_required",
                    "awaiting_confirmation",
                ])),
            },
            Step::Confirm {
                turns: vec![ModelTurn::Text("submitted for confirmation")],
                outcome: ExpectedStream::done(&["message_started", "tool_result", "delta", "done"]),
            },
            Step::CompleteBackend,
            Step::Send {
                text: "check the run",
                turns: vec![
                    ModelTurn::Tool {
                        id: "status",
                        name: "run_status",
                        arguments: Arguments::LastRun,
                    },
                    ModelTurn::Text("run completed"),
                ],
                outcome: SendOutcome::Stream(ExpectedStream::done(&[
                    "message_started",
                    "tool_call",
                    "tool_result",
                    "delta",
                    "done",
                ])),
            },
        ],
        expected: ExpectedFinal {
            messages: [
                vec![
                    ExpectedMessage::user(),
                    ExpectedMessage::assistant(),
                    ExpectedMessage::tool(),
                    ExpectedMessage::assistant(),
                    ExpectedMessage::tool(),
                    ExpectedMessage::assistant(),
                    ExpectedMessage::tool(),
                    ExpectedMessage::assistant(),
                    ExpectedMessage::tool(),
                    ExpectedMessage::assistant(),
                ],
                vec![
                    ExpectedMessage::user(),
                    ExpectedMessage::assistant(),
                    ExpectedMessage::tool(),
                    ExpectedMessage::assistant(),
                ],
            ]
            .concat(),
            audits: vec![
                ExpectedAudit::automatic("list_workflows", true),
                ExpectedAudit::automatic("inspect_workflow", true),
                ExpectedAudit::automatic("propose_run", true),
                ExpectedAudit::decision("submit_run", "pending", None, "succeeded"),
                ExpectedAudit::decision("submit_run", "invocation_started", None, "succeeded"),
                ExpectedAudit::decision(
                    "submit_run",
                    "invocation_succeeded",
                    Some(true),
                    "succeeded",
                ),
                ExpectedAudit::automatic("run_status", true),
            ],
            prompt_calls: 1,
            runs: 1,
        },
    };
    harness.execute(&scenario).await;
}

#[tokio::test]
async fn invalid_arguments_are_returned_to_the_model_and_never_executed() {
    let mut harness = ScenarioHarness::start("invalid arguments", true).await;
    let scenario = Scenario {
        name: "invalid arguments",
        supports_tools: true,
        steps: vec![Step::Send {
            text: "inspect with an invalid request",
            turns: vec![
                ModelTurn::Tool {
                    id: "invalid",
                    name: "inspect_workflow",
                    arguments: Arguments::Exact(json!({"workflow_id": 7})),
                },
                ModelTurn::Text("I handled the validation error"),
            ],
            outcome: SendOutcome::Stream(ExpectedStream::done(&[
                "message_started",
                "tool_call",
                "tool_result",
                "delta",
                "done",
            ])),
        }],
        expected: ExpectedFinal {
            messages: confirmed_messages(1),
            audits: vec![ExpectedAudit::invalid("inspect_workflow")],
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
    let requests = harness.llm.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "invalid"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("expected string"))
        }));
}

#[tokio::test]
async fn rejecting_confirmation_creates_no_run() {
    let mut harness = ScenarioHarness::start("confirmation reject", true).await;
    let arguments = harness.run_arguments(21, 8);
    let scenario = Scenario {
        name: "confirmation reject",
        supports_tools: true,
        steps: vec![
            Step::Send {
                text: "prepare a run",
                turns: vec![ModelTurn::Tool {
                    id: "submit-reject",
                    name: "submit_run",
                    arguments: Arguments::Exact(arguments),
                }],
                outcome: SendOutcome::Stream(ExpectedStream::awaiting(&[
                    "message_started",
                    "tool_call",
                    "confirmation_required",
                    "awaiting_confirmation",
                ])),
            },
            Step::Reject {
                turns: vec![ModelTurn::Text("the run was rejected")],
                outcome: ExpectedStream::done(&["message_started", "tool_result", "delta", "done"]),
            },
        ],
        expected: ExpectedFinal {
            messages: vec![
                ExpectedMessage::user(),
                ExpectedMessage::assistant(),
                ExpectedMessage::tool(),
                ExpectedMessage::assistant(),
            ],
            audits: vec![
                ExpectedAudit::decision("submit_run", "pending", None, "rejected"),
                ExpectedAudit::decision("submit_run", "rejected", None, "rejected"),
            ],
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
}

#[tokio::test]
async fn the_tool_loop_guard_terminates_with_a_failed_state() {
    let mut harness = ScenarioHarness::start("loop guard", true).await;
    let turns = (0..8)
        .map(|index| ModelTurn::Tool {
            id: match index {
                0 => "loop-0",
                1 => "loop-1",
                2 => "loop-2",
                3 => "loop-3",
                4 => "loop-4",
                5 => "loop-5",
                6 => "loop-6",
                _ => "loop-7",
            },
            name: "list_workflows",
            arguments: Arguments::Exact(json!({})),
        })
        .collect();
    let scenario = Scenario {
        name: "loop guard",
        supports_tools: true,
        steps: vec![Step::Send {
            text: "keep using tools",
            turns,
            outcome: SendOutcome::Stream(ExpectedStream::error(
                &[
                    "message_started",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "tool_call",
                    "tool_result",
                    "error",
                ],
                AssistantMessageStatus::Failed,
            )),
        }],
        expected: ExpectedFinal {
            messages: ExpectedMessage::sequence(8, AssistantMessageStatus::Failed),
            audits: (0..8)
                .map(|_| ExpectedAudit::automatic("list_workflows", true))
                .collect(),
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
    let detail = harness
        .client
        .assistant_conversation(
            &harness.conversation_id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert!(detail
        .messages
        .last()
        .unwrap()
        .error
        .as_deref()
        .is_some_and(|error| { error.contains("maximum of 8 model steps") }));
}

#[tokio::test]
async fn edit_uses_a_fresh_preview_token_and_exact_edited_values() {
    let mut harness = ScenarioHarness::start("preview edit", true).await;
    let original = harness.run_arguments(31, 10);
    let edited = harness.run_arguments(41, 12);
    let scenario = Scenario {
        name: "preview edit",
        supports_tools: true,
        steps: vec![
            Step::Send {
                text: "prepare the original run",
                turns: vec![ModelTurn::Tool {
                    id: "submit-edit",
                    name: "submit_run",
                    arguments: Arguments::Exact(original),
                }],
                outcome: SendOutcome::Stream(ExpectedStream::awaiting(&[
                    "message_started",
                    "tool_call",
                    "confirmation_required",
                    "awaiting_confirmation",
                ])),
            },
            Step::Preview {
                arguments: edited.clone(),
            },
            Step::Edit {
                arguments: edited,
                turns: vec![ModelTurn::Text("edited run submitted")],
                outcome: ExpectedStream::done(&["message_started", "tool_result", "delta", "done"]),
            },
        ],
        expected: ExpectedFinal {
            messages: vec![
                ExpectedMessage::user(),
                ExpectedMessage::assistant(),
                ExpectedMessage::tool(),
                ExpectedMessage::assistant(),
            ],
            audits: vec![
                ExpectedAudit::decision("submit_run", "pending", None, "succeeded"),
                ExpectedAudit::decision("submit_run", "invocation_started", None, "succeeded"),
                ExpectedAudit::decision(
                    "submit_run",
                    "invocation_succeeded",
                    Some(true),
                    "succeeded",
                ),
            ],
            prompt_calls: 1,
            runs: 1,
        },
    };
    harness.execute(&scenario).await;
    let graph = harness
        .api
        .fake
        .submitted_graph
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(graph["3"]["inputs"]["seed"], json!(41));
    assert_eq!(graph["3"]["inputs"]["steps"], json!(12));
}

#[tokio::test]
async fn stale_preview_token_is_refused_and_confirmation_stays_pending() {
    let mut harness = ScenarioHarness::start("stale preview", true).await;
    let original = harness.run_arguments(51, 14);
    let edited = harness.run_arguments(52, 16);
    let scenario = Scenario {
        name: "stale preview",
        supports_tools: true,
        steps: vec![
            Step::Send {
                text: "prepare a run to edit",
                turns: vec![ModelTurn::Tool {
                    id: "submit-stale",
                    name: "submit_run",
                    arguments: Arguments::Exact(original),
                }],
                outcome: SendOutcome::Stream(ExpectedStream::awaiting(&[
                    "message_started",
                    "tool_call",
                    "confirmation_required",
                    "awaiting_confirmation",
                ])),
            },
            Step::Preview {
                arguments: edited.clone(),
            },
            Step::StaleEdit {
                arguments: edited.clone(),
                status: StatusCode::UNPROCESSABLE_ENTITY,
            },
            Step::Preview { arguments: edited },
            Step::Reject {
                turns: vec![ModelTurn::Text("stale edit safely rejected")],
                outcome: ExpectedStream::done(&["message_started", "tool_result", "delta", "done"]),
            },
        ],
        expected: ExpectedFinal {
            messages: vec![
                ExpectedMessage::user(),
                ExpectedMessage::assistant(),
                ExpectedMessage::tool(),
                ExpectedMessage::assistant(),
            ],
            audits: vec![
                ExpectedAudit::decision("submit_run", "pending", None, "rejected"),
                ExpectedAudit::decision("submit_run", "rejected", None, "rejected"),
            ],
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
}

#[tokio::test]
async fn double_confirm_creates_exactly_one_run() {
    let mut harness = ScenarioHarness::start("double confirm", true).await;
    let arguments = harness.run_arguments(61, 18);
    let scenario = Scenario {
        name: "double confirm",
        supports_tools: true,
        steps: vec![
            Step::Send {
                text: "prepare one run",
                turns: vec![ModelTurn::Tool {
                    id: "submit-twice",
                    name: "submit_run",
                    arguments: Arguments::Exact(arguments),
                }],
                outcome: SendOutcome::Stream(ExpectedStream::awaiting(&[
                    "message_started",
                    "tool_call",
                    "confirmation_required",
                    "awaiting_confirmation",
                ])),
            },
            Step::Confirm {
                turns: vec![ModelTurn::Text("the run was confirmed once")],
                outcome: ExpectedStream::done(&["message_started", "tool_result", "delta", "done"]),
            },
            Step::ConfirmAgain {
                status: StatusCode::CONFLICT,
            },
        ],
        expected: ExpectedFinal {
            messages: vec![
                ExpectedMessage::user(),
                ExpectedMessage::assistant(),
                ExpectedMessage::tool(),
                ExpectedMessage::assistant(),
            ],
            audits: vec![
                ExpectedAudit::decision("submit_run", "pending", None, "succeeded"),
                ExpectedAudit::decision("submit_run", "invocation_started", None, "succeeded"),
                ExpectedAudit::decision(
                    "submit_run",
                    "invocation_succeeded",
                    Some(true),
                    "succeeded",
                ),
            ],
            prompt_calls: 1,
            runs: 1,
        },
    };
    harness.execute(&scenario).await;
}

#[tokio::test]
async fn cancellation_during_stream_persists_partial_response_without_submission() {
    let mut harness = ScenarioHarness::start("stream cancellation", true).await;
    let scenario = Scenario {
        name: "stream cancellation",
        supports_tools: true,
        steps: vec![Step::Cancel {
            text: "start a long response",
            partial: "partial response",
            outcome: ExpectedStream::error(
                &["message_started", "delta", "error"],
                AssistantMessageStatus::Cancelled,
            ),
        }],
        expected: ExpectedFinal {
            messages: vec![
                ExpectedMessage::user(),
                ExpectedMessage::assistant_status(AssistantMessageStatus::Cancelled),
            ],
            audits: vec![],
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
    let detail = harness
        .client
        .assistant_conversation(
            &harness.conversation_id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        &detail.messages[1].content,
        genaimage_api::AssistantChatMessage::Assistant { text, .. } if text == "partial response"
    ));
}

#[tokio::test]
async fn a_model_without_tool_support_fails_clearly_before_streaming() {
    let mut harness = ScenarioHarness::start("unsupported tools", false).await;
    let scenario = Scenario {
        name: "unsupported tools",
        supports_tools: false,
        steps: vec![Step::Send {
            text: "list workflows",
            turns: vec![],
            outcome: SendOutcome::Problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "lacks the required tools capability",
            ),
        }],
        expected: ExpectedFinal {
            messages: vec![],
            audits: vec![],
            prompt_calls: 0,
            runs: 0,
        },
    };
    harness.execute(&scenario).await;
    assert_eq!(harness.llm.chat_request_count(), 0);
}

fn loopback_url(value: &str) -> Url {
    let url = Url::parse(value).expect("GENAIMAGE_LIVE_LLM_URL must be an absolute URL");
    let loopback = match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    assert!(loopback, "live LLM tests refuse non-loopback hosts: {url}");
    url
}

fn quoted_toml_key(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[tokio::test]
#[ignore = "requires a local Ollama server; excluded from CI"]
async fn live_ollama_stream_terminates_without_unconfirmed_submission() {
    if std::env::var("GENAIMAGE_LIVE_LLM").ok().as_deref() != Some("1") {
        return;
    }
    let base = std::env::var("GENAIMAGE_LIVE_LLM_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
    loopback_url(&base);
    let text_model = std::env::var("GENAIMAGE_LIVE_LLM_TEXT_MODEL")
        .expect("set GENAIMAGE_LIVE_LLM_TEXT_MODEL for a live test");
    let vision_model = std::env::var("GENAIMAGE_LIVE_LLM_VISION_MODEL").ok();
    let shared_model = vision_model.as_deref() == Some(text_model.as_str());
    let mut capability_config = format!(
        "[llm.capabilities.\"{}\"]\ntools = true\n{}",
        quoted_toml_key(&text_model),
        if shared_model { "vision = true\n" } else { "" }
    );
    if let Some(model) = vision_model
        .as_deref()
        .filter(|model| !shared_model || *model != text_model)
    {
        capability_config.push_str(&format!(
            "[llm.capabilities.\"{}\"]\ntools = false\nvision = true\n",
            quoted_toml_key(model)
        ));
    }
    let mut env = vec![
        ("GENAIMAGE_LLM_URL", base.as_str()),
        ("GENAIMAGE_LLM_TEXT_MODEL", text_model.as_str()),
    ];
    if let Some(model) = vision_model.as_deref() {
        env.push(("GENAIMAGE_LLM_VISION_MODEL", model));
    }
    let api =
        ApiHarness::start_with_builtin_assistant_tools_and_extra(30, &env, &capability_config)
            .await;
    install_workflow(&api);
    let client = Client::with_base_url(&api.origin).unwrap();
    let conversation = client
        .create_assistant_conversation(&CreateAssistantConversationRequest {
            title: "Live Ollama safety".into(),
        })
        .await
        .unwrap();
    let stream = client
        .send_assistant_message(
            &conversation.id,
            &message(
                "Inspect the available workflows and prepare, but never submit, a generation run.",
            ),
        )
        .await
        .unwrap();
    let first = observe_with_timeout(stream, Duration::from_secs(120)).await;
    assert!(
        first.terminal.is_some(),
        "live stream needs a terminal event"
    );
    assert!(
        first.confirmation.is_some()
            == matches!(
                first.terminal.as_ref().map(|terminal| terminal.kind),
                Some(TerminalKind::AwaitingConfirmation)
            ),
        "confirmation proposals must end in awaiting_confirmation"
    );
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);

    if let Some(pending) = first.confirmation {
        let continuation = client
            .decide_assistant_confirmation(
                &conversation.id,
                &pending.id,
                &AssistantConfirmationRequest {
                    decision: AssistantConfirmationDecisionKind::Reject,
                    arguments: None,
                    reason: Some("live safety test rejection".into()),
                    preview_token: None,
                },
            )
            .await
            .unwrap();
        let rejected = observe_with_timeout(continuation, Duration::from_secs(120)).await;
        assert!(rejected.terminal.is_some());
    }

    let detail = client
        .assistant_conversation(
            &conversation.id,
            &AssistantConversationDetailQuery::default(),
        )
        .await
        .unwrap();
    assert!(!detail.messages.is_empty());
    assert!(detail.messages.iter().all(|entry| !entry.id.is_empty()));
    let audit = client
        .assistant_tool_calls(&conversation.id, &AssistantToolAuditQuery::default())
        .await
        .unwrap();
    assert!(audit.tool_calls.iter().all(|entry| !entry.tool.is_empty()));
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert!(api.core.list_runs(100).unwrap().is_empty());
}
