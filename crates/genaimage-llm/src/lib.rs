//! OpenAI-compatible HTTP adapter for local LLM servers.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error as StdError,
    fmt,
    net::ToSocketAddrs,
    pin::Pin,
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use eventsource_stream::Eventsource;
use futures_util::{stream, Stream, StreamExt};
use genaimage_core::{
    async_trait,
    config::{validate_endpoint, validate_endpoint_addresses},
    extensions::{
        ChatContent, ChatDelta, ChatMessage, ChatRequest, ChatStream, LlmModel, ToolCall,
    },
    HarnessError, LlmProvider, Result,
};
use reqwest::{Client, Response, Url};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::OnceCell;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_MESSAGE: usize = 1024;
const MAX_MODELS_BODY_BYTES: usize = 512 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 256 * 1024;
const MAX_PENDING_TOOL_CALLS: usize = 64;
const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_TOOL_ID_BYTES: usize = 1024;
const MAX_TOOL_NAME_BYTES: usize = 1024;
const ENDPOINT_KEY: &str = "llm.base_url";

/// Connection and streaming options for an OpenAI-compatible local endpoint.
///
/// `allowed_hosts` is for exact private or Compose host names. Public hosts are
/// rejected even when they are listed.
#[derive(Clone)]
pub struct OpenAiCompatibleProviderOptions {
    pub base_url: String,
    pub api_key: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub connect_timeout: Duration,
    pub first_byte_timeout: Duration,
    pub idle_timeout: Duration,
}

impl fmt::Debug for OpenAiCompatibleProviderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleProviderOptions")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("allowed_hosts", &self.allowed_hosts)
            .field("connect_timeout", &self.connect_timeout)
            .field("first_byte_timeout", &self.first_byte_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .finish()
    }
}

impl Default for OpenAiCompatibleProviderOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            allowed_hosts: Vec::new(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }
}

/// An OpenAI-compatible provider for an explicitly selected local model.
#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    base_url: Url,
    options: OpenAiCompatibleProviderOptions,
    connection: std::sync::Arc<OnceCell<PinnedConnection>>,
}

impl fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleProvider")
            .field("base_url", &self.base_url)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PinnedConnection {
    http: Client,
}

impl OpenAiCompatibleProvider {
    /// Validates the configured endpoint and constructs the provider. DNS and
    /// resolved-address validation happen before the first request.
    pub fn new(options: OpenAiCompatibleProviderOptions) -> Result<Self> {
        if options.connect_timeout.is_zero()
            || options.first_byte_timeout.is_zero()
            || options.idle_timeout.is_zero()
        {
            return Err(HarnessError::Configuration(
                "LLM provider timeouts must be greater than zero".to_owned(),
            ));
        }
        let mut base_url =
            validate_endpoint(ENDPOINT_KEY, &options.base_url, &options.allowed_hosts)?;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            base_url,
            options,
            connection: std::sync::Arc::new(OnceCell::new()),
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    fn error(&self, message: impl Into<String>) -> HarnessError {
        provider_error(message, self.options.api_key.as_deref())
    }

    fn redact(&self, message: impl Into<String>) -> String {
        redact(self.options.api_key.as_deref(), message.into())
    }

    async fn connection(&self) -> Result<&PinnedConnection> {
        self.connection
            .get_or_try_init(|| async {
                let host = self.base_url.host_str().expect("validated host");
                let port = self
                    .base_url
                    .port_or_known_default()
                    .expect("validated scheme");
                let resolution = tokio::task::spawn_blocking({
                    let host = host.to_owned();
                    move || {
                        (host.as_str(), port)
                            .to_socket_addrs()
                            .map(|addresses| addresses.collect::<Vec<_>>())
                            .map_err(|error| error.to_string())
                    }
                });
                let addresses = tokio::time::timeout(self.options.connect_timeout, resolution)
                    .await
                    .map_err(|_| {
                        self.error(format!("endpoint DNS resolution to {host} timed out"))
                    })?
                    .map_err(|error| {
                        self.error(format!("cannot start endpoint DNS resolution: {error}"))
                    })?
                    .map_err(|error| {
                        self.error(format!(
                            "endpoint {host} is unavailable: cannot resolve host ({error})"
                        ))
                    })?;
                validate_endpoint_addresses(
                    ENDPOINT_KEY,
                    &self.options.base_url,
                    &self.options.allowed_hosts,
                    &addresses,
                )?;
                let http = Client::builder()
                    .retry(reqwest::retry::never())
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(self.options.connect_timeout)
                    .resolve_to_addrs(host, &addresses)
                    .build()
                    .map_err(|error| self.error(format!("cannot build HTTP client: {error}")))?;
                Ok(PinnedConnection { http })
            })
            .await
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(HarnessError::Url)
    }

    fn request(
        &self,
        client: &Client,
        method: reqwest::Method,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let request = client.request(method, url);
        match &self.options.api_key {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response> {
        tokio::time::timeout(self.options.first_byte_timeout, request.send())
            .await
            .map_err(|_| self.error("request timed out waiting for the first response byte"))?
            .map_err(|error| map_request_error(error, self.options.api_key.as_deref()))
    }

    async fn reject_error_response(
        &self,
        response: Response,
        operation: &str,
        capabilities: RequestCapabilities<'_>,
    ) -> HarnessError {
        let status = response.status();
        let message = read_error_message(
            response,
            self.options.idle_timeout,
            self.options.api_key.as_deref(),
        )
        .await;
        let lower = message.to_ascii_lowercase();
        if capabilities.images && is_capability_message(&lower, &["vision", "image", "multimodal"])
        {
            return HarnessError::Configuration(self.redact(format!(
                "model '{}' does not support the requested vision/image capability: HTTP {status}: {message}",
                capabilities.model
            )));
        }
        if capabilities.tools && is_capability_message(&lower, &["tool", "function call"]) {
            return HarnessError::Configuration(self.redact(format!(
                "model '{}' does not support the requested tool-calling capability: HTTP {status}: {message}",
                capabilities.model
            )));
        }
        self.error(format!("{operation} returned HTTP {status}: {message}"))
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let model = request.model.clone();
        let has_images = request.messages.iter().any(|message| {
            matches!(message, ChatMessage::User(content) if content.iter().any(|part| matches!(part, ChatContent::Image { .. })))
        });
        let has_tools = !request.tools.is_empty();
        let body = encode_request(&request)?;
        let response = self
            .send(
                self.request(
                    &self.connection().await?.http,
                    reqwest::Method::POST,
                    self.endpoint("chat/completions")?,
                )
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(&body),
            )
            .await?;
        if !response.status().is_success() {
            return Err(self
                .reject_error_response(
                    response,
                    "chat completion",
                    RequestCapabilities {
                        model: &model,
                        tools: has_tools,
                        images: has_images,
                    },
                )
                .await);
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err(self.error("chat completion did not return a text/event-stream response"));
        }

        Ok(decode_chat_stream(
            response,
            model,
            self.options.idle_timeout,
            self.options.api_key.clone(),
        ))
    }

    async fn list_models(&self) -> Result<Vec<LlmModel>> {
        let response = self
            .send(
                self.request(
                    &self.connection().await?.http,
                    reqwest::Method::GET,
                    self.endpoint("models")?,
                )
                .header(reqwest::header::ACCEPT, "application/json"),
            )
            .await?;
        if !response.status().is_success() {
            return Err(self
                .reject_error_response(
                    response,
                    "model listing",
                    RequestCapabilities {
                        model: "(model listing)",
                        tools: false,
                        images: false,
                    },
                )
                .await);
        }
        let bytes = read_limited_body(
            response,
            self.options.first_byte_timeout,
            MAX_MODELS_BODY_BYTES,
            "model listing",
            self.options.api_key.as_deref(),
        )
        .await?;
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| self.error(format!("model listing returned invalid JSON: {error}")))?;
        let models = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| self.error("model listing JSON is missing the 'data' array"))?;
        models
            .iter()
            .map(|model| {
                let id = model
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| self.error("model listing contains a model without an ID"))?;
                Ok(LlmModel {
                    id: id.to_owned(),
                    supports_tools: capability(model, "tools"),
                    supports_vision: capability(model, "vision"),
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
struct RequestCapabilities<'a> {
    model: &'a str,
    tools: bool,
    images: bool,
}

fn capability(model: &Value, name: &str) -> bool {
    model
        .get(format!("supports_{name}"))
        .or_else(|| model.pointer(&format!("/capabilities/{name}")))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn is_capability_message(message: &str, names: &[&str]) -> bool {
    names.iter().any(|name| message.contains(name))
        && [
            "unsupported",
            "support",
            "invalid",
            "unknown",
            "not available",
        ]
        .iter()
        .any(|word| message.contains(word))
}

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
}

fn encode_request(request: &ChatRequest) -> Result<Value> {
    let messages = request
        .messages
        .iter()
        .map(|message| match message {
            ChatMessage::System(text) => json!({ "role": "system", "content": text }),
            ChatMessage::User(parts) => {
                let content = parts
                    .iter()
                    .map(|part| match part {
                        ChatContent::Text(text) => json!({ "type": "text", "text": text }),
                        ChatContent::Image { media_type, bytes } => json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{media_type};base64,{}", STANDARD.encode(bytes))
                            }
                        }),
                    })
                    .collect::<Vec<_>>();
                json!({ "role": "user", "content": content })
            }
            ChatMessage::Assistant { text, tool_calls } => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text.clone()) }
                });
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(tool_calls.iter().map(|call| json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string()
                        }
                    })).collect());
                }
                message
            },
            ChatMessage::Tool { call_id, result } => json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result.as_str().map(str::to_owned).unwrap_or_else(|| result.to_string())
            }),
        })
        .collect();
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        })
        .collect();
    serde_json::to_value(OpenAiRequest {
        model: request.model.clone(),
        messages,
        stream: true,
        tools,
    })
    .map_err(HarnessError::Json)
}

type ByteStream =
    Pin<Box<dyn Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>>;
type SseStream = Pin<
    Box<
        dyn Stream<
                Item = std::result::Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<TimedBodyError>,
                >,
            > + Send,
    >,
>;

#[derive(Default)]
struct SseLimitState {
    event_bytes: usize,
    line_bytes: usize,
    pending_cr: bool,
}

fn with_idle_timeout(
    source: ByteStream,
    idle_timeout: Duration,
) -> impl Stream<Item = std::result::Result<bytes::Bytes, TimedBodyError>> {
    stream::unfold(
        (Some(source.fuse()), SseLimitState::default()),
        move |(source, mut limit)| async move {
            let mut source = source?;
            match tokio::time::timeout(idle_timeout, source.next()).await {
                Ok(Some(Ok(bytes))) => {
                    if sse_event_exceeds_limit(&bytes, &mut limit) {
                        return Some((
                            Err(TimedBodyError::EventTooLarge(MAX_SSE_EVENT_BYTES)),
                            (None, limit),
                        ));
                    }
                    Some((Ok(bytes), (Some(source), limit)))
                }
                Ok(Some(Err(error))) => Some((Err(TimedBodyError::Request(error)), (None, limit))),
                Ok(None) => None,
                Err(_) => Some((Err(TimedBodyError::Idle(idle_timeout)), (None, limit))),
            }
        },
    )
}

fn sse_event_exceeds_limit(chunk: &[u8], state: &mut SseLimitState) -> bool {
    for byte in chunk {
        if state.pending_cr {
            state.pending_cr = false;
            if *byte == b'\n' {
                continue;
            }
        }
        match *byte {
            b'\r' => {
                state.event_bytes = state.event_bytes.saturating_add(1);
                if state.line_bytes == 0 {
                    state.event_bytes = 0;
                }
                state.line_bytes = 0;
                state.pending_cr = true;
            }
            b'\n' => {
                state.event_bytes = state.event_bytes.saturating_add(1);
                if state.line_bytes == 0 {
                    state.event_bytes = 0;
                }
                state.line_bytes = 0;
            }
            _ => {
                state.event_bytes = state.event_bytes.saturating_add(1);
                state.line_bytes = state.line_bytes.saturating_add(1);
            }
        }
        if state.event_bytes > MAX_SSE_EVENT_BYTES {
            return true;
        }
    }
    false
}

fn decode_chat_stream(
    response: Response,
    model: String,
    idle_timeout: Duration,
    api_key: Option<String>,
) -> ChatStream {
    let source = Box::pin(response.bytes_stream()) as ByteStream;
    let events = Box::pin(with_idle_timeout(source, idle_timeout).eventsource()) as SseStream;
    let state = DecodeChatState {
        events: Some(events),
        parser: ChatParser::new(model, api_key),
        pending: VecDeque::new(),
    };
    Box::pin(stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        loop {
            if let Some(item) = state.pending.pop_front() {
                return Some((item, Some(state)));
            }
            let Some(event) = state.events.as_mut()?.next().await else {
                state.pending.extend(state.parser.finish());
                state.events = None;
                continue;
            };
            match event {
                Ok(event) => state
                    .pending
                    .extend(state.parser.process_event(&event.data)),
                Err(error) => {
                    state.parser.fail();
                    state.pending.push_back(Err(state
                        .parser
                        .error(format!("malformed SSE response: {error}"))));
                }
            }
            if state.parser.ended {
                state.events = None;
            }
            if state.pending.is_empty() && state.parser.ended {
                return None;
            }
        }
    }))
}

struct DecodeChatState {
    events: Option<SseStream>,
    parser: ChatParser,
    pending: VecDeque<Result<ChatDelta>>,
}

#[derive(Debug)]
enum TimedBodyError {
    Request(reqwest::Error),
    Idle(Duration),
    EventTooLarge(usize),
}

impl fmt::Display for TimedBodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => write!(formatter, "{error}"),
            Self::Idle(duration) => {
                write!(formatter, "stream idle for {} ms", duration.as_millis())
            }
            Self::EventTooLarge(maximum) => {
                write!(formatter, "SSE event exceeds the {maximum} byte limit")
            }
        }
    }
}

impl StdError for TimedBodyError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::Idle(_) | Self::EventTooLarge(_) => None,
        }
    }
}

struct ChatParser {
    model: String,
    api_key: Option<String>,
    tool_calls: BTreeMap<usize, PendingToolCall>,
    ended: bool,
}

impl fmt::Debug for ChatParser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatParser")
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("tool_calls", &self.tool_calls.len())
            .field("ended", &self.ended)
            .finish()
    }
}

#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ChatParser {
    fn new(model: String, api_key: Option<String>) -> Self {
        Self {
            model,
            api_key,
            tool_calls: BTreeMap::new(),
            ended: false,
        }
    }

    fn error(&self, message: impl Into<String>) -> HarnessError {
        provider_error(message, self.api_key.as_deref())
    }

    fn process_event(&mut self, data: &str) -> Vec<Result<ChatDelta>> {
        if self.ended {
            return Vec::new();
        }
        if data.len() > MAX_SSE_EVENT_BYTES {
            self.fail();
            return vec![Err(self.error(format!(
                "malformed SSE event for model '{}': event exceeds the {} byte limit",
                self.model, MAX_SSE_EVENT_BYTES
            )))];
        }
        if data.trim() == "[DONE]" {
            self.ended = true;
            return self.take_tool_calls();
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(error) => {
                self.ended = true;
                return vec![Err(self.error(format!(
                    "malformed SSE data for model '{}': invalid JSON ({error})",
                    self.model
                )))];
            }
        };
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            self.ended = true;
            return vec![Err(self.error(format!(
                "malformed SSE data for model '{}': missing choices array",
                self.model
            )))];
        };
        let mut output = Vec::new();
        for choice in choices {
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = delta.get("content").and_then(Value::as_str) {
                    if !text.is_empty() {
                        output.push(Ok(ChatDelta::Text(text.to_owned())));
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        if let Err(error) = self.accumulate_tool_call(call) {
                            self.ended = true;
                            output.push(Err(error));
                            return output;
                        }
                    }
                }
            }
            if choice
                .get("finish_reason")
                .is_some_and(|value| !value.is_null())
            {
                self.ended = true;
                output.append(&mut self.take_tool_calls());
            }
        }
        output
    }

    fn accumulate_tool_call(&mut self, call: &Value) -> Result<()> {
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| self.error("malformed tool-call delta: missing non-negative index"))?;
        let is_new = !self.tool_calls.contains_key(&index);
        if is_new && self.tool_calls.len() >= MAX_PENDING_TOOL_CALLS {
            return Err(self.error(format!(
                "malformed SSE data for model '{}': pending tool-call count exceeds the {MAX_PENDING_TOOL_CALLS} call limit",
                self.model
            )));
        }
        let api_key = self.api_key.clone();
        let pending = self.tool_calls.entry(index).or_default();
        if let Some(id) = call.get("id").and_then(Value::as_str) {
            append_bounded(&mut pending.id, id, MAX_TOOL_ID_BYTES, "tool-call id")
                .map_err(|message| provider_error(message, api_key.as_deref()))?;
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                append_bounded(
                    &mut pending.name,
                    name,
                    MAX_TOOL_NAME_BYTES,
                    "tool-call function name",
                )
                .map_err(|message| provider_error(message, api_key.as_deref()))?;
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                append_bounded(
                    &mut pending.arguments,
                    arguments,
                    MAX_TOOL_ARGUMENT_BYTES,
                    "tool-call arguments",
                )
                .map_err(|message| provider_error(message, api_key.as_deref()))?;
            }
        }
        Ok(())
    }

    fn fail(&mut self) {
        self.ended = true;
        self.tool_calls.clear();
    }

    fn finish(&mut self) -> Vec<Result<ChatDelta>> {
        if self.ended {
            return Vec::new();
        }
        let model = self.model.clone();
        self.fail();
        vec![Err(self.error(format!(
            "malformed SSE stream for model '{model}': ended without [DONE] or a terminal finish_reason"
        )))]
    }

    fn take_tool_calls(&mut self) -> Vec<Result<ChatDelta>> {
        if self.tool_calls.is_empty() {
            return Vec::new();
        }
        let calls = std::mem::take(&mut self.tool_calls);
        calls
            .into_iter()
            .map(|(index, call)| {
                if call.id.is_empty() || call.name.is_empty() {
                    return Err(self.error(format!(
                        "malformed tool call {index}: completed delta is missing an id or function name"
                    )));
                }
                let arguments = serde_json::from_str(&call.arguments).map_err(|error| {
                    self.error(format!(
                        "tool call '{}' for model '{}' has invalid JSON arguments: {error}; fragment={}",
                        call.id,
                        self.model,
                        truncate(&call.arguments, 256)
                    ))
                })?;
                Ok(ChatDelta::ToolCall(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments,
                }))
            })
            .collect()
    }
}

fn map_request_error(error: reqwest::Error, api_key: Option<&str>) -> HarnessError {
    if error.is_timeout() {
        provider_error(format!("request timed out: {error}"), api_key)
    } else if error.is_connect() {
        provider_error(format!("endpoint is unavailable: {error}"), api_key)
    } else {
        provider_error(format!("request failed: {error}"), api_key)
    }
}

async fn read_error_message(
    response: Response,
    timeout: Duration,
    api_key: Option<&str>,
) -> String {
    let status = response.status();
    match tokio::time::timeout(timeout, async {
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk[..chunk.len().min(MAX_ERROR_MESSAGE * 4 - body.len())]);
            if body.len() >= MAX_ERROR_MESSAGE * 4 {
                break;
            }
        }
        Ok::<_, reqwest::Error>(body)
    })
    .await
    {
        Ok(Ok(bytes)) => {
            let text = String::from_utf8_lossy(&bytes);
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .or_else(|| value.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| text.into_owned());
            truncate(
                redact(api_key, message.trim().to_owned()).trim(),
                MAX_ERROR_MESSAGE,
            )
        }
        Ok(Err(error)) => redact(
            api_key,
            format!("could not read HTTP {status} response body: {error}"),
        ),
        Err(_) => format!("timed out reading HTTP {status} response body"),
    }
}

async fn read_limited_body(
    response: Response,
    timeout: Duration,
    maximum: usize,
    operation: &str,
    api_key: Option<&str>,
) -> Result<Vec<u8>> {
    tokio::time::timeout(timeout, async {
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(ResultReadError::Http)? {
            if body.len().saturating_add(chunk.len()) > maximum {
                return Err(ResultReadError::Provider(provider_error(
                    format!("{operation} response exceeds the {maximum} byte limit"),
                    api_key,
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
    .map_err(|_| provider_error(format!("{operation} response timed out"), api_key))?
    .map_err(|error: ResultReadError| match error {
        ResultReadError::Http(error) => provider_error(
            format!("cannot read {operation} response: {error}"),
            api_key,
        ),
        ResultReadError::Provider(error) => error,
    })
}

#[derive(Debug)]
enum ResultReadError {
    Http(reqwest::Error),
    Provider(HarnessError),
}

fn append_bounded(
    target: &mut String,
    fragment: &str,
    maximum: usize,
    field: &str,
) -> std::result::Result<(), String> {
    if target.len().saturating_add(fragment.len()) > maximum {
        return Err(format!("{field} exceeds the {maximum} byte limit"));
    }
    target.push_str(fragment);
    Ok(())
}

fn truncate(text: &str, maximum: usize) -> String {
    let mut output = String::with_capacity(maximum.min(text.len()));
    for (index, character) in text.char_indices() {
        if index >= maximum {
            output.push('…');
            break;
        }
        output.push(character);
    }
    output
}

fn redact(api_key: Option<&str>, message: String) -> String {
    match api_key.filter(|key| !key.is_empty()) {
        Some(key) => message.replace(key, "[REDACTED]"),
        None => message,
    }
}

fn provider_error(message: impl Into<String>, api_key: Option<&str>) -> HarnessError {
    HarnessError::Backend {
        backend: "OpenAI-compatible LLM".to_owned(),
        message: redact(api_key, message.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_have_documented_defaults() {
        let options = OpenAiCompatibleProviderOptions::default();
        assert_eq!(options.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(options.connect_timeout, Duration::from_secs(5));
        assert_eq!(options.first_byte_timeout, Duration::from_secs(60));
        assert_eq!(options.idle_timeout, Duration::from_secs(30));
    }

    #[test]
    fn sse_limiter_resets_on_mixed_logical_line_endings() {
        for blank_line in [&b"\r\n\n"[..], &b"\n\r\n"[..]] {
            let mut state = SseLimitState::default();
            assert!(!sse_event_exceeds_limit(
                &vec![b'x'; MAX_SSE_EVENT_BYTES - 10],
                &mut state,
            ));
            assert!(!sse_event_exceeds_limit(blank_line, &mut state));
            assert!(!sse_event_exceeds_limit(
                &vec![b'x'; MAX_SSE_EVENT_BYTES - 10],
                &mut state,
            ));
        }
    }

    #[test]
    fn public_endpoint_is_rejected_at_construction() {
        let result = OpenAiCompatibleProvider::new(OpenAiCompatibleProviderOptions {
            base_url: "http://8.8.8.8/v1".to_owned(),
            allowed_hosts: vec!["8.8.8.8".to_owned()],
            ..Default::default()
        });
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;

    #[test]
    fn provider_debug_redacts_api_key() {
        let options = OpenAiCompatibleProviderOptions {
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key: Some("provider-debug-secret".into()),
            ..OpenAiCompatibleProviderOptions::default()
        };
        let provider = OpenAiCompatibleProvider::new(options.clone()).unwrap();
        let parser = ChatParser::new("model".into(), Some("provider-debug-secret".into()));
        for debug in [
            format!("{options:?}"),
            format!("{provider:?}"),
            format!("{parser:?}"),
        ] {
            assert!(!debug.contains("provider-debug-secret"), "{debug}");
            assert!(debug.contains("<redacted>"), "{debug}");
        }
    }
}
