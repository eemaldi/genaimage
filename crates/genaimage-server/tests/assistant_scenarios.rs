mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::atomic::Ordering,
    time::Duration,
};

use common::{install_workflow, ApiHarness};
use futures_util::StreamExt;
use genaimage_api::{
    client::{AssistantEventStream, AssistantStreamEvent, Client, Error},
    AssistantChatContent, AssistantChatMessage, AssistantConfirmationDecisionKind,
    AssistantConfirmationPreviewRequest, AssistantConfirmationRequest,
    AssistantConversationDetailQuery, AssistantMessageRole, AssistantMessageStatus,
    AssistantToolAuditQuery, AssistantToolCall, AssistantToolConfirmationStatus,
    CreateAssistantConversationRequest, SendAssistantMessageRequest,
};
use genaimage_test_support::{FakeLlmEvent, FakeLlmScript, FakeLlmServer, FakeToolCallDelta};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::time::timeout;
use url::{Host, Url};

const MODEL: &str = "scenario-model";
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(10);

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
    tool_results: BTreeMap<&'static str, ExpectedToolResult>,
    previews: BTreeMap<&'static str, Value>,
    prompt_calls: usize,
    runs: usize,
}

#[derive(Clone, Debug)]
enum ExpectedToolResult {
    Exact {
        ok: bool,
        result: Option<Value>,
        error: Option<String>,
    },
    SubmittedRun {
        status: &'static str,
    },
    RunStatus {
        status: &'static str,
        output_count: usize,
    },
}

impl ExpectedToolResult {
    fn success(result: Value) -> Self {
        Self::Exact {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn error(error: &str) -> Self {
        Self::Exact {
            ok: false,
            result: None,
            error: Some(error.to_owned()),
        }
    }
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
    tool: String,
    tool_call_id: String,
    arguments: Value,
}

#[derive(Clone, Debug)]
enum ResolvedTurn {
    Text(String),
    Tool {
        id: String,
        name: String,
        arguments: Value,
    },
}

#[derive(Clone, Debug)]
struct ObservedToolCall {
    conversation_id: String,
    user_message_id: String,
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Clone, Debug)]
struct ObservedToolResult {
    conversation_id: String,
    user_message_id: String,
    call_id: String,
    tool: String,
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct ObservedConfirmation {
    id: String,
    conversation_id: String,
    user_message_id: String,
    tool_call_id: String,
    tool: String,
    arguments: Value,
    preview: Option<Value>,
    token: String,
}

#[derive(Debug)]
struct TerminalObservation {
    kind: TerminalKind,
    status: AssistantMessageStatus,
    conversation_id: String,
    user_message_id: String,
    message_id: Option<String>,
}

#[derive(Debug)]
struct Observation {
    events: Vec<&'static str>,
    started_conversation_id: Option<String>,
    started_user_message_id: Option<String>,
    deltas: Vec<String>,
    tool_calls: Vec<ObservedToolCall>,
    tool_results: Vec<ObservedToolResult>,
    confirmation: Option<ObservedConfirmation>,
    terminal_confirmation_id: Option<String>,
    terminal: Option<TerminalObservation>,
}

impl Observation {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            started_conversation_id: None,
            started_user_message_id: None,
            deltas: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            confirmation: None,
            terminal_confirmation_id: None,
            terminal: None,
        }
    }
}

#[derive(Clone, Debug)]
struct StreamRecord {
    user_text: Option<String>,
    turns: Vec<ResolvedTurn>,
    confirmation: Option<ObservedConfirmation>,
    terminal_status: AssistantMessageStatus,
    terminal_message_id: String,
    expected_tool_results: BTreeMap<String, Value>,
    continuation_call_id: Option<String>,
}

#[derive(Clone, Debug)]
struct DecisionRecord {
    confirmation_id: String,
    decision: &'static str,
    effective_arguments: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PersistedMessageSnapshot {
    role: AssistantMessageRole,
    status: AssistantMessageStatus,
    content: Value,
    tool_call_id: Option<String>,
    error: Option<String>,
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
    stream_records: Vec<StreamRecord>,
    decisions: Vec<DecisionRecord>,
    expected_tool_results: BTreeMap<&'static str, ExpectedToolResult>,
    expected_previews: BTreeMap<&'static str, Value>,
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
            stream_records: Vec::new(),
            decisions: Vec::new(),
            expected_tool_results: BTreeMap::new(),
            expected_previews: BTreeMap::new(),
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

    fn resolve_turns(&self, turns: &[ModelTurn]) -> Vec<ResolvedTurn> {
        turns
            .iter()
            .map(|turn| match turn {
                ModelTurn::Text(text) => ResolvedTurn::Text((*text).to_owned()),
                ModelTurn::Tool {
                    id,
                    name,
                    arguments,
                } => ResolvedTurn::Tool {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    arguments: arguments.resolve(self),
                },
            })
            .collect()
    }

    fn set_scripts(&self, turns: &[ModelTurn]) {
        if !turns.is_empty() {
            self.llm.set_scripts(self.model_scripts(turns));
        }
    }

    fn record_stream(
        &mut self,
        user_text: Option<&str>,
        turns: &[ModelTurn],
        continuation_call_id: Option<String>,
        observation: Observation,
    ) {
        self.record_resolved_stream(
            user_text,
            self.resolve_turns(turns),
            continuation_call_id,
            observation,
        );
    }

    fn record_resolved_stream(
        &mut self,
        user_text: Option<&str>,
        resolved: Vec<ResolvedTurn>,
        continuation_call_id: Option<String>,
        observation: Observation,
    ) {
        let expected_tool_results =
            self.assert_typed_stream(&resolved, continuation_call_id.as_deref(), &observation);
        let terminal = observation.terminal.as_ref().expect("terminal event");
        self.stream_records.push(StreamRecord {
            user_text: user_text.map(str::to_owned),
            turns: resolved,
            confirmation: observation.confirmation,
            terminal_status: terminal.status,
            terminal_message_id: terminal
                .message_id
                .clone()
                .expect("persisted terminal message ID"),
            expected_tool_results,
            continuation_call_id,
        });
    }

    fn assert_typed_stream(
        &self,
        turns: &[ResolvedTurn],
        continuation_call_id: Option<&str>,
        observation: &Observation,
    ) -> BTreeMap<String, Value> {
        let conversation_id = observation
            .started_conversation_id
            .as_deref()
            .expect("message_started conversation ID");
        let user_message_id = observation
            .started_user_message_id
            .as_deref()
            .expect("message_started user message ID");
        assert_eq!(conversation_id, self.conversation_id);
        assert!(!user_message_id.is_empty());

        let expected_calls = turns
            .iter()
            .filter_map(|turn| match turn {
                ResolvedTurn::Tool {
                    id,
                    name,
                    arguments,
                } => Some((id, name, arguments)),
                ResolvedTurn::Text(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(observation.tool_calls.len(), expected_calls.len());
        for (actual, (id, name, arguments)) in observation
            .tool_calls
            .iter()
            .zip(expected_calls.iter().copied())
        {
            assert_eq!(actual.conversation_id, conversation_id);
            assert_eq!(actual.user_message_id, user_message_id);
            assert_eq!(&actual.id, id);
            assert_eq!(&actual.name, name);
            assert_eq!(&actual.arguments, arguments);
        }

        let expected_deltas = turns
            .iter()
            .filter_map(|turn| match turn {
                ResolvedTurn::Text(text) => Some(text.clone()),
                ResolvedTurn::Tool { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observation.deltas, expected_deltas,
            "scripted delta content"
        );

        let mut expected_tool_results = BTreeMap::new();
        for result in &observation.tool_results {
            assert_eq!(result.conversation_id, conversation_id);
            assert_eq!(result.user_message_id, user_message_id);
            let linked = observation
                .tool_calls
                .iter()
                .any(|call| call.id == result.call_id);
            if continuation_call_id != Some(result.call_id.as_str()) {
                assert!(
                    linked,
                    "tool result {} did not link to a call",
                    result.call_id
                );
            } else if let Some(pending) = self.pending.as_ref() {
                assert_eq!(pending.tool_call_id, result.call_id);
                assert_eq!(pending.tool, result.tool);
            }
            let call_arguments = expected_calls
                .iter()
                .find(|(id, _, _)| id.as_str() == result.call_id)
                .map(|(_, _, arguments)| (*arguments).clone())
                .or_else(|| {
                    continuation_call_id
                        .filter(|id| *id == result.call_id)
                        .and_then(|_| {
                            self.pending
                                .as_ref()
                                .map(|pending| pending.arguments.clone())
                        })
                });
            let expected = self
                .expected_tool_results
                .get(result.call_id.as_str())
                .unwrap_or_else(|| {
                    panic!("missing declared result for tool call {}", result.call_id)
                });
            let normalized =
                self.assert_expected_tool_result(result, call_arguments.as_ref(), expected);
            expected_tool_results.insert(result.call_id.clone(), normalized);
        }

        if let Some(confirmation) = &observation.confirmation {
            assert_eq!(confirmation.conversation_id, conversation_id);
            assert_eq!(confirmation.user_message_id, user_message_id);
            assert!(!confirmation.id.is_empty());
            assert!(!confirmation.token.is_empty());
            let call = observation
                .tool_calls
                .iter()
                .find(|call| call.id == confirmation.tool_call_id)
                .expect("confirmation must reference its tool call");
            assert_eq!(confirmation.tool, call.name);
            assert_eq!(confirmation.arguments, call.arguments);
            let preview = confirmation.preview.as_ref().expect("submit preview");
            let expected_preview = self
                .expected_previews
                .get(confirmation.tool_call_id.as_str())
                .unwrap_or_else(|| {
                    panic!("missing declared preview for {}", confirmation.tool_call_id)
                });
            assert_eq!(preview, expected_preview);
        }

        if let Some(confirmation_id) = observation.terminal_confirmation_id.as_deref() {
            assert_eq!(
                confirmation_id,
                observation
                    .confirmation
                    .as_ref()
                    .expect("terminal confirmation payload")
                    .id
            );
        }
        let terminal = observation.terminal.as_ref().expect("terminal event");
        assert_eq!(terminal.conversation_id, conversation_id);
        assert_eq!(terminal.user_message_id, user_message_id);
        assert!(terminal
            .message_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()));
        expected_tool_results
    }

    fn assert_expected_tool_result(
        &self,
        actual: &ObservedToolResult,
        call_arguments: Option<&Value>,
        expected: &ExpectedToolResult,
    ) -> Value {
        match expected {
            ExpectedToolResult::Exact { ok, result, error } => {
                assert_eq!(actual.ok, *ok);
                assert_eq!(actual.result, *result);
                assert_eq!(actual.error, *error);
                if *ok {
                    json!({"ok": true, "result": result.clone().unwrap()})
                } else {
                    json!({"ok": false, "error": error.clone().unwrap()})
                }
            }
            ExpectedToolResult::SubmittedRun { status } => {
                assert!(actual.ok);
                assert!(actual.error.is_none());
                let result = actual.result.as_ref().expect("submitted run result");
                let run_id = result["run_id"].as_str().expect("submitted run ID");
                assert!(!run_id.is_empty());
                assert_eq!(result["status"], *status);
                assert!(
                    self.api.core.get_run(run_id).is_ok(),
                    "run_id must link to persisted run"
                );
                json!({"ok": true, "result": {"run_id": run_id, "status": status}})
            }
            ExpectedToolResult::RunStatus {
                status,
                output_count,
            } => {
                assert!(actual.ok);
                assert!(actual.error.is_none());
                let result = actual.result.as_ref().expect("run status result");
                let arguments = call_arguments.expect("run_status arguments");
                assert_eq!(result["run_id"], arguments["run_id"]);
                assert_eq!(result["status"], *status);
                assert_eq!(
                    result["outputs"].as_array().expect("outputs array").len(),
                    *output_count
                );
                assert!(result["outputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|output| output["run_id"] == result["run_id"]));
                json!({
                    "ok": true,
                    "result": {
                        "run_id": arguments["run_id"],
                        "status": status,
                        "output_count": output_count
                    }
                })
            }
        }
    }

    async fn execute(&mut self, scenario: &Scenario) {
        timeout(SCENARIO_TIMEOUT, self.execute_inner(scenario))
            .await
            .unwrap_or_else(|_| {
                panic!("scenario '{}' exceeded {SCENARIO_TIMEOUT:?}", scenario.name)
            });
    }

    async fn execute_inner(&mut self, scenario: &Scenario) {
        assert_eq!(scenario.name, self.name);
        assert_eq!(scenario.supports_tools, self.supports_tools);
        self.expected_tool_results = scenario.expected.tool_results.clone();
        self.expected_previews = scenario.expected.previews.clone();
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
                if let Some(confirmation) = observation.confirmation.as_ref() {
                    let pending = PendingConfirmation {
                        id: confirmation.id.clone(),
                        token: confirmation.token.clone(),
                        tool: confirmation.tool.clone(),
                        tool_call_id: confirmation.tool_call_id.clone(),
                        arguments: confirmation.arguments.clone(),
                    };
                    self.latest_preview_token = Some(pending.token.clone());
                    self.last_confirmation = Some(pending.clone());
                    self.pending = Some(pending);
                }
                self.record_stream(Some(text), turns, None, observation);
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
                &AssistantConfirmationPreviewRequest {
                    arguments: arguments.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(preview.preview, Some(run_preview(&arguments)));
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
        let observation = observe(stream).await;
        expected.assert(&observation);
        self.decisions.push(DecisionRecord {
            confirmation_id: pending.id.clone(),
            decision: "confirm",
            effective_arguments: Some(pending.arguments.clone()),
        });
        self.record_stream(None, turns, Some(pending.tool_call_id.clone()), observation);
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
                    arguments: Some(arguments.clone()),
                    reason: None,
                    preview_token: Some(token),
                },
            )
            .await
            .unwrap();
        let observation = observe(stream).await;
        expected.assert(&observation);
        self.decisions.push(DecisionRecord {
            confirmation_id: pending.id.clone(),
            decision: "edit",
            effective_arguments: Some(arguments),
        });
        self.record_stream(None, turns, Some(pending.tool_call_id.clone()), observation);
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
        let observation = observe(stream).await;
        expected.assert(&observation);
        self.decisions.push(DecisionRecord {
            confirmation_id: pending.id.clone(),
            decision: "reject",
            effective_arguments: None,
        });
        self.record_stream(None, turns, Some(pending.tool_call_id.clone()), observation);
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
        let mut observation = Observation::new();
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
        self.record_resolved_stream(
            Some(text),
            vec![ResolvedTurn::Text(partial.to_owned())],
            None,
            observation,
        );
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

    fn declared_tool_calls(&self) -> BTreeMap<String, (&str, &Value)> {
        self.stream_records
            .iter()
            .flat_map(|record| record.turns.iter())
            .filter_map(|turn| match turn {
                ResolvedTurn::Tool {
                    id,
                    name,
                    arguments,
                } => Some((id.clone(), (name.as_str(), arguments))),
                ResolvedTurn::Text(_) => None,
            })
            .collect()
    }

    fn normalise_persisted_tool_result(
        &self,
        call_id: &str,
        actual_result: Option<Value>,
        actual_error: Option<String>,
        actual_ok: Option<bool>,
    ) -> Value {
        let (_, (tool, arguments)) = self
            .declared_tool_calls()
            .into_iter()
            .find(|(id, _)| id == call_id)
            .expect("persisted tool call must be declared");
        let expected = self
            .expected_tool_results
            .get(call_id)
            .expect("persisted tool result must be declared");
        self.assert_expected_tool_result(
            &ObservedToolResult {
                conversation_id: self.conversation_id.clone(),
                user_message_id: String::new(),
                call_id: call_id.to_owned(),
                tool: tool.to_owned(),
                ok: actual_ok.expect("persisted tool result must record ok"),
                result: actual_result,
                error: actual_error,
            },
            Some(arguments),
            expected,
        )
    }

    fn expected_persisted_messages(&self) -> (Vec<PersistedMessageSnapshot>, Vec<(usize, String)>) {
        let mut messages = Vec::new();
        let mut terminal_ids = Vec::new();
        for record in &self.stream_records {
            if let Some(text) = &record.user_text {
                messages.push(PersistedMessageSnapshot {
                    role: AssistantMessageRole::User,
                    status: AssistantMessageStatus::Complete,
                    content: serde_json::to_value(AssistantChatMessage::User(vec![
                        AssistantChatContent::Text(text.clone()),
                    ]))
                    .unwrap(),
                    tool_call_id: None,
                    error: None,
                });
            }
            if let Some(call_id) = &record.continuation_call_id {
                let outcome = record
                    .expected_tool_results
                    .get(call_id)
                    .expect("continuation expected tool result");
                messages.push(tool_message_snapshot(call_id, outcome));
            }
            for turn in &record.turns {
                match turn {
                    ResolvedTurn::Tool {
                        id,
                        name,
                        arguments,
                    } => {
                        let awaiting = record
                            .confirmation
                            .as_ref()
                            .is_some_and(|confirmation| confirmation.tool_call_id == *id);
                        messages.push(PersistedMessageSnapshot {
                            role: AssistantMessageRole::Assistant,
                            status: AssistantMessageStatus::Complete,
                            content: serde_json::to_value(AssistantChatMessage::Assistant {
                                text: String::new(),
                                tool_calls: vec![AssistantToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                }],
                            })
                            .unwrap(),
                            tool_call_id: None,
                            error: None,
                        });
                        if !awaiting {
                            let outcome = record
                                .expected_tool_results
                                .get(id)
                                .expect("automatic expected tool result");
                            messages.push(tool_message_snapshot(id, outcome));
                        }
                    }
                    ResolvedTurn::Text(text) => messages.push(PersistedMessageSnapshot {
                        role: AssistantMessageRole::Assistant,
                        status: record.terminal_status,
                        content: serde_json::to_value(AssistantChatMessage::Assistant {
                            text: text.clone(),
                            tool_calls: Vec::new(),
                        })
                        .unwrap(),
                        tool_call_id: None,
                        error: None,
                    }),
                }
            }
            if record.terminal_status == AssistantMessageStatus::Failed
                && matches!(record.turns.last(), Some(ResolvedTurn::Tool { .. }) | None)
            {
                messages.push(PersistedMessageSnapshot {
                    role: AssistantMessageRole::Assistant,
                    status: AssistantMessageStatus::Failed,
                    content: serde_json::to_value(AssistantChatMessage::Assistant {
                        text: String::new(),
                        tool_calls: Vec::new(),
                    })
                    .unwrap(),
                    tool_call_id: None,
                    error: Some(
                        "Assistant tool loop exceeded the maximum of 8 model steps for this turn."
                            .into(),
                    ),
                });
            }
            terminal_ids.push((messages.len() - 1, record.terminal_message_id.clone()));
        }
        (messages, terminal_ids)
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
        let (expected_snapshots, terminal_ids) = self.expected_persisted_messages();
        let actual_snapshots = detail
            .messages
            .iter()
            .map(|message| {
                let mut content = serde_json::to_value(&message.content).unwrap();
                if message.role == AssistantMessageRole::Tool {
                    let call_id = message.tool_call_id.as_deref().expect("tool call ID");
                    let outcome = content["tool"]["result"].clone();
                    let normalised = self.normalise_persisted_tool_result(
                        call_id,
                        outcome.get("result").cloned(),
                        outcome
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        outcome.get("ok").and_then(Value::as_bool),
                    );
                    content["tool"]["result"] = normalised;
                }
                PersistedMessageSnapshot {
                    role: message.role,
                    status: message.status,
                    content,
                    tool_call_id: message.tool_call_id.clone(),
                    error: message.error.clone(),
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual_snapshots, expected_snapshots,
            "{} persisted message fields",
            self.name
        );
        for (index, message_id) in terminal_ids {
            assert_eq!(
                detail.messages[index].id, message_id,
                "terminal SSE message must be the persisted terminal message"
            );
        }
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
        let known_calls = self.declared_tool_calls();
        let known_results = self
            .stream_records
            .iter()
            .flat_map(|record| record.expected_tool_results.iter())
            .map(|(id, result)| (id.clone(), result.clone()))
            .collect::<BTreeMap<_, _>>();
        let confirmations = self
            .stream_records
            .iter()
            .filter_map(|record| record.confirmation.as_ref())
            .map(|confirmation| (confirmation.id.as_str(), confirmation))
            .collect::<std::collections::BTreeMap<_, _>>();
        for row in &audit {
            let (call_name, call_arguments) = known_calls
                .get(&row.tool_call_id)
                .expect("audit call must link to a declared tool call");
            assert_eq!(row.tool, *call_name);
            assert_eq!(row.proposed_arguments, **call_arguments);
            if row.invocation_ok.is_some() {
                let actual_outcome = self.normalise_persisted_tool_result(
                    &row.tool_call_id,
                    row.invocation_result.clone(),
                    row.invocation_error.clone(),
                    row.invocation_ok,
                );
                assert_eq!(actual_outcome, known_results[&row.tool_call_id]);
            }
            match row.decision.as_str() {
                "automatic" => {
                    let outcome = &known_results[&row.tool_call_id];
                    assert_eq!(row.validation_outcome, "valid");
                    assert!(row.validation_error.is_none());
                    assert_eq!(row.effective_arguments, Some((*call_arguments).clone()));
                    assert!(row.decided_by.is_none());
                    assert_eq!(row.invocation_ok, outcome["ok"].as_bool());
                    assert_eq!(
                        row.invocation_error,
                        outcome
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    );
                    assert!(row.confirmation_id.is_none());
                }
                "not_invoked" => {
                    let outcome = &known_results[&row.tool_call_id];
                    assert_eq!(row.validation_outcome, "invalid");
                    assert_eq!(
                        row.validation_error,
                        outcome
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    );
                    assert_eq!(row.invocation_ok, Some(false));
                    assert!(row.effective_arguments.is_none());
                    assert!(row.decided_by.is_none());
                    assert!(row.invocation_result.is_none());
                    assert_eq!(
                        row.invocation_error,
                        outcome
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    );
                    assert!(row.confirmation_id.is_none());
                }
                "pending" => {
                    let confirmation = confirmations
                        .values()
                        .find(|confirmation| confirmation.tool_call_id == row.tool_call_id)
                        .expect("pending confirmation");
                    assert_eq!(
                        row.confirmation_id.as_deref(),
                        Some(confirmation.id.as_str())
                    );
                    assert_eq!(row.validation_outcome, "valid");
                    assert!(row.validation_error.is_none());
                    assert!(row.effective_arguments.is_none());
                    assert!(row.decided_by.is_none());
                    assert!(row.invocation_ok.is_none());
                }
                "invocation_started" | "invocation_succeeded" | "rejected" => {
                    let confirmation_id = row
                        .confirmation_id
                        .as_deref()
                        .expect("decision audit confirmation ID");
                    let confirmation = confirmations[confirmation_id];
                    assert_eq!(confirmation.tool_call_id, row.tool_call_id);
                    let decision = self
                        .decisions
                        .iter()
                        .find(|decision| decision.confirmation_id == confirmation_id)
                        .expect("decision record");
                    assert_eq!(row.validation_outcome, "valid");
                    assert!(row.validation_error.is_none());
                    assert_eq!(row.effective_arguments, decision.effective_arguments);
                    match row.decision.as_str() {
                        "invocation_started" => {
                            assert_eq!(row.decided_by.as_deref(), Some("user"));
                            assert!(row.invocation_ok.is_none());
                            assert!(row.invocation_result.is_none());
                            assert!(row.invocation_error.is_none());
                        }
                        "rejected" => {
                            assert_eq!(decision.decision, "reject");
                            assert_eq!(row.decided_by.as_deref(), Some("user"));
                            assert!(row.invocation_ok.is_none());
                            assert!(row.invocation_result.is_none());
                            assert!(row.invocation_error.is_none());
                        }
                        "invocation_succeeded" => {
                            assert!(matches!(decision.decision, "confirm" | "edit"));
                            assert_eq!(row.decided_by.as_deref(), Some("runtime"));
                            let outcome = &known_results[&row.tool_call_id];
                            assert_eq!(row.invocation_ok, Some(true));
                            assert_eq!(
                                row.invocation_error,
                                outcome
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            );
                        }
                        _ => unreachable!(),
                    }
                }
                other => panic!("unexpected audit decision {other}"),
            }
        }
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
    let mut observation = Observation::new();
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
        AssistantStreamEvent::MessageStarted(event) => {
            observation.events.push("message_started");
            assert!(observation.started_conversation_id.is_none());
            observation.started_conversation_id = Some(event.conversation_id);
            observation.started_user_message_id = Some(event.user_message_id);
            assert_eq!(event.model, MODEL);
        }
        AssistantStreamEvent::Delta(event) => {
            observation.events.push("delta");
            assert_eq!(
                Some(&event.conversation_id),
                observation.started_conversation_id.as_ref()
            );
            assert_eq!(
                Some(&event.user_message_id),
                observation.started_user_message_id.as_ref()
            );
            observation.deltas.push(event.text);
        }
        AssistantStreamEvent::ToolCall(event) => {
            observation.events.push("tool_call");
            assert_eq!(
                Some(&event.conversation_id),
                observation.started_conversation_id.as_ref()
            );
            assert_eq!(
                Some(&event.user_message_id),
                observation.started_user_message_id.as_ref()
            );
            observation.tool_calls.push(ObservedToolCall {
                conversation_id: event.conversation_id,
                user_message_id: event.user_message_id,
                id: event.tool_call.id,
                name: event.tool_call.name,
                arguments: event.tool_call.arguments,
            });
        }
        AssistantStreamEvent::ToolResult(event) => {
            observation.events.push("tool_result");
            assert_eq!(
                Some(&event.conversation_id),
                observation.started_conversation_id.as_ref()
            );
            assert_eq!(
                Some(&event.user_message_id),
                observation.started_user_message_id.as_ref()
            );
            observation.tool_results.push(ObservedToolResult {
                conversation_id: event.conversation_id,
                user_message_id: event.user_message_id,
                call_id: event.tool_call_id,
                tool: event.tool,
                ok: event.ok,
                result: event.result,
                error: event.error,
            });
        }
        AssistantStreamEvent::ConfirmationRequired(event) => {
            observation.events.push("confirmation_required");
            assert_eq!(event.status, AssistantToolConfirmationStatus::Pending);
            assert_eq!(
                Some(&event.conversation_id),
                observation.started_conversation_id.as_ref()
            );
            observation.confirmation = Some(ObservedConfirmation {
                id: event.confirmation_id,
                conversation_id: event.conversation_id,
                user_message_id: observation
                    .started_user_message_id
                    .clone()
                    .expect("message_started before confirmation"),
                tool_call_id: event.tool_call_id,
                tool: event.tool,
                arguments: event.arguments,
                preview: event.preview,
                token: event.preview_token,
            });
        }
        AssistantStreamEvent::AwaitingConfirmation(event) => {
            observation.events.push("awaiting_confirmation");
            assert_eq!(event.status, AssistantMessageStatus::AwaitingConfirmation);
            observation.terminal_confirmation_id = Some(event.confirmation_id);
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::AwaitingConfirmation,
                status: event.status,
                conversation_id: event.conversation_id,
                user_message_id: event.user_message_id,
                message_id: Some(event.message_id),
            });
        }
        AssistantStreamEvent::Done(event) => {
            observation.events.push("done");
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::Done,
                status: event.status,
                conversation_id: event.conversation_id,
                user_message_id: event.user_message_id,
                message_id: event.message_id,
            });
        }
        AssistantStreamEvent::Error(event) => {
            observation.events.push("error");
            observation.terminal = Some(TerminalObservation {
                kind: TerminalKind::Error,
                status: event.status,
                conversation_id: event.conversation_id,
                user_message_id: event.user_message_id,
                message_id: event.message_id,
            });
        }
    }
}

fn tool_message_snapshot(call_id: &str, outcome: &Value) -> PersistedMessageSnapshot {
    PersistedMessageSnapshot {
        role: AssistantMessageRole::Tool,
        status: AssistantMessageStatus::Complete,
        content: serde_json::to_value(AssistantChatMessage::Tool {
            call_id: call_id.to_owned(),
            result: outcome.clone(),
        })
        .unwrap(),
        tool_call_id: Some(call_id.to_owned()),
        error: None,
    }
}

fn workflow_list_result(revision: &str) -> Value {
    json!({
        "workflows": [{
            "id": "contract-workflow",
            "name": "Contract workflow",
            "description": null,
            "tags": [],
            "revision": revision
        }],
        "truncated": false
    })
}

fn inspect_workflow_result(revision: &str) -> Value {
    let common = |kind: &str, required: bool| {
        json!({
            "type": kind,
            "required": required,
            "description": "",
            "default": null,
            "allowed_values": null,
            "minimum": null,
            "maximum": null,
            "widget": null,
            "step": null,
            "options": null,
            "group": null,
            "advanced": null,
            "placeholder": null,
            "user_supplied": false
        })
    };
    let mut seed = common("seed", true);
    let mut steps = common("integer", true);
    seed["minimum"] = Value::Null;
    steps["minimum"] = json!(1);
    steps["maximum"] = json!(100);
    json!({
        "workflow_id": "contract-workflow",
        "revision": revision,
        "parameters": {"seed": seed, "steps": steps},
        "presets": {"fast": {"seed": 42, "steps": 10}}
    })
}

fn run_preview(arguments: &Value) -> Value {
    json!({
        "workflow_id": arguments["workflow_id"],
        "workflow_revision": arguments["workflow_revision"],
        "preset": arguments["preset"],
        "parameters": arguments["parameters"],
        "issues": []
    })
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

async fn run_scripted_scenario<F>(name: &'static str, test: F)
where
    F: Future<Output = ()>,
{
    timeout(SCENARIO_TIMEOUT, test)
        .await
        .unwrap_or_else(|_| panic!("scenario '{name}' exceeded {SCENARIO_TIMEOUT:?}"));
}

fn confirmed_messages(turns: usize) -> Vec<ExpectedMessage> {
    ExpectedMessage::sequence(turns, AssistantMessageStatus::Complete)
}

#[tokio::test]
async fn multi_step_tools_require_confirmation_then_check_run_status() {
    run_scripted_scenario(
        "multi_step_tools_require_confirmation_then_check_run_status",
        async {
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
                                arguments: Arguments::Exact(
                                    json!({"workflow_id": "contract-workflow"}),
                                ),
                            },
                            ModelTurn::Tool {
                                id: "propose",
                                name: "propose_run",
                                arguments: Arguments::Exact(arguments.clone()),
                            },
                            ModelTurn::Tool {
                                id: "submit",
                                name: "submit_run",
                                arguments: Arguments::Exact(arguments.clone()),
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
                        outcome: ExpectedStream::done(&[
                            "message_started",
                            "tool_result",
                            "delta",
                            "done",
                        ]),
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
                        ExpectedAudit::decision(
                            "submit_run",
                            "invocation_started",
                            None,
                            "succeeded",
                        ),
                        ExpectedAudit::decision(
                            "submit_run",
                            "invocation_succeeded",
                            Some(true),
                            "succeeded",
                        ),
                        ExpectedAudit::automatic("run_status", true),
                    ],
                    tool_results: BTreeMap::from([
                        (
                            "list",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "inspect",
                            ExpectedToolResult::success(inspect_workflow_result(&harness.revision)),
                        ),
                        (
                            "propose",
                            ExpectedToolResult::success(run_preview(&arguments)),
                        ),
                        (
                            "submit",
                            ExpectedToolResult::SubmittedRun { status: "queued" },
                        ),
                        (
                            "status",
                            ExpectedToolResult::RunStatus {
                                status: "completed",
                                output_count: 1,
                            },
                        ),
                    ]),
                    previews: BTreeMap::from([("submit", run_preview(&arguments))]),
                    prompt_calls: 1,
                    runs: 1,
                },
            };
            harness.execute(&scenario).await;
        },
    )
    .await;
}

#[tokio::test]
async fn invalid_arguments_are_returned_to_the_model_and_never_executed() {
    run_scripted_scenario(
        "invalid_arguments_are_returned_to_the_model_and_never_executed",
        async {
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
                    tool_results: BTreeMap::from([(
                        "invalid",
                        ExpectedToolResult::error("$.workflow_id: expected string"),
                    )]),
                    previews: BTreeMap::new(),
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
        },
    )
    .await;
}

#[tokio::test]
async fn rejecting_confirmation_creates_no_run() {
    run_scripted_scenario("rejecting_confirmation_creates_no_run", async {
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
                        arguments: Arguments::Exact(arguments.clone()),
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
                    outcome: ExpectedStream::done(&[
                        "message_started",
                        "tool_result",
                        "delta",
                        "done",
                    ]),
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
                tool_results: BTreeMap::from([(
                    "submit-reject",
                    ExpectedToolResult::error(
                        "The user rejected tool 'submit_run'. Reason: declined by scenario",
                    ),
                )]),
                previews: BTreeMap::from([("submit-reject", run_preview(&arguments))]),
                prompt_calls: 0,
                runs: 0,
            },
        };
        harness.execute(&scenario).await;
    })
    .await;
}

#[tokio::test]
async fn the_tool_loop_guard_terminates_with_a_failed_state() {
    run_scripted_scenario(
        "the_tool_loop_guard_terminates_with_a_failed_state",
        async {
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
                    tool_results: BTreeMap::from([
                        (
                            "loop-0",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-1",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-2",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-3",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-4",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-5",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-6",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                        (
                            "loop-7",
                            ExpectedToolResult::success(workflow_list_result(&harness.revision)),
                        ),
                    ]),
                    previews: BTreeMap::new(),
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
        },
    )
    .await;
}

#[tokio::test]
async fn edit_uses_a_fresh_preview_token_and_exact_edited_values() {
    run_scripted_scenario(
        "edit_uses_a_fresh_preview_token_and_exact_edited_values",
        async {
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
                            arguments: Arguments::Exact(original.clone()),
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
                        arguments: edited.clone(),
                        turns: vec![ModelTurn::Text("edited run submitted")],
                        outcome: ExpectedStream::done(&[
                            "message_started",
                            "tool_result",
                            "delta",
                            "done",
                        ]),
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
                        ExpectedAudit::decision(
                            "submit_run",
                            "invocation_started",
                            None,
                            "succeeded",
                        ),
                        ExpectedAudit::decision(
                            "submit_run",
                            "invocation_succeeded",
                            Some(true),
                            "succeeded",
                        ),
                    ],
                    tool_results: BTreeMap::from([(
                        "submit-edit",
                        ExpectedToolResult::SubmittedRun { status: "queued" },
                    )]),
                    previews: BTreeMap::from([("submit-edit", run_preview(&original))]),
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
            assert_eq!(graph["3"]["inputs"]["seed"], edited["parameters"]["seed"]);
            assert_eq!(graph["3"]["inputs"]["steps"], edited["parameters"]["steps"]);
            let run = harness.api.core.list_runs(10).unwrap().pop().unwrap();
            assert_eq!(run.parameters, edited["parameters"]);
        },
    )
    .await;
}

#[tokio::test]
async fn stale_preview_token_is_refused_and_confirmation_stays_pending() {
    run_scripted_scenario(
        "stale_preview_token_is_refused_and_confirmation_stays_pending",
        async {
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
                            arguments: Arguments::Exact(original.clone()),
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
                        outcome: ExpectedStream::done(&[
                            "message_started",
                            "tool_result",
                            "delta",
                            "done",
                        ]),
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
                    tool_results: BTreeMap::from([(
                        "submit-stale",
                        ExpectedToolResult::error(
                            "The user rejected tool 'submit_run'. Reason: declined by scenario",
                        ),
                    )]),
                    previews: BTreeMap::from([("submit-stale", run_preview(&original))]),
                    prompt_calls: 0,
                    runs: 0,
                },
            };
            harness.execute(&scenario).await;
        },
    )
    .await;
}

#[tokio::test]
async fn double_confirm_creates_exactly_one_run() {
    run_scripted_scenario("double_confirm_creates_exactly_one_run", async {
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
                        arguments: Arguments::Exact(arguments.clone()),
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
                    outcome: ExpectedStream::done(&[
                        "message_started",
                        "tool_result",
                        "delta",
                        "done",
                    ]),
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
                tool_results: BTreeMap::from([(
                    "submit-twice",
                    ExpectedToolResult::SubmittedRun { status: "queued" },
                )]),
                previews: BTreeMap::from([("submit-twice", run_preview(&arguments))]),
                prompt_calls: 1,
                runs: 1,
            },
        };
        harness.execute(&scenario).await;
    })
    .await;
}

#[tokio::test]
async fn cancellation_during_stream_persists_partial_response_without_submission() {
    run_scripted_scenario("cancellation_during_stream_persists_partial_response_without_submission", async {

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
            tool_results: BTreeMap::new(),
            previews: BTreeMap::new(),
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

    }).await;
}

#[tokio::test]
async fn a_model_without_tool_support_fails_clearly_before_streaming() {
    run_scripted_scenario(
        "a_model_without_tool_support_fails_clearly_before_streaming",
        async {
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
                    tool_results: BTreeMap::new(),
                    previews: BTreeMap::new(),
                    prompt_calls: 0,
                    runs: 0,
                },
            };
            harness.execute(&scenario).await;
            assert_eq!(harness.llm.chat_request_count(), 0);
        },
    )
    .await;
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
    let seconds = std::env::var("GENAIMAGE_LIVE_LLM_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(240);
    timeout(Duration::from_secs(seconds), live_ollama_scenario())
        .await
        .unwrap_or_else(|_| panic!("live Ollama scenario exceeded {seconds} seconds"));
}

async fn live_ollama_scenario() {
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
