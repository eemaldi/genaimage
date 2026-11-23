use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    multipart::{Form, Part},
    Client, Url,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::{
    sync::{mpsc, watch, OnceCell},
    time::{sleep, timeout},
};
use tokio_tungstenite::{client_async_tls_with_config, tungstenite::Message};

use genaimage_core::{
    async_trait,
    config::ComfyUiProfile,
    image_backend::{
        BackendEvent, BackendHealth, EventMonitor, HistoryOutcome, HistorySnapshot, ImageBackend,
        OutputReference, PromptResponse, QueueSnapshot, UploadResponse,
    },
    HarnessError, Result,
};

const MAX_SAFE_READ_ATTEMPTS: usize = 3;
type DnsAnswer = std::result::Result<Vec<SocketAddr>, String>;
type DnsReceiver = watch::Receiver<Option<DnsAnswer>>;

#[derive(Clone, Debug)]
pub struct ComfyUiClient {
    base_url: Url,
    profile: ComfyUiProfile,
    connection: Arc<OnceCell<PinnedConnection>>,
    dns: Arc<Mutex<Option<DnsReceiver>>>,
    #[cfg(test)]
    test_addresses: Option<Arc<std::sync::Mutex<Vec<SocketAddr>>>>,
}

#[derive(Debug)]
struct PinnedConnection {
    http: Client,
    addresses: Vec<SocketAddr>,
}

impl ComfyUiClient {
    pub fn new(profile: &ComfyUiProfile) -> Result<Self> {
        Ok(Self {
            base_url: profile.endpoint_url()?,
            profile: profile.clone(),
            connection: Arc::new(OnceCell::new()),
            dns: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_addresses: None,
        })
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.profile.request_timeout_seconds)
    }

    async fn connection(&self) -> Result<&PinnedConnection> {
        self.connection.get_or_try_init(|| async {
            #[cfg(test)]
            if let Some(addresses) = &self.test_addresses {
                return self.pinned_connection(addresses.lock().unwrap().clone());
            }
            let host = self.base_url.host_str().expect("validated host").trim_matches(['[', ']']);
            let port = self.base_url.port_or_known_default().expect("validated scheme");
            let deadline = Duration::from_secs(self.profile.request_timeout_seconds.min(3));
            let mut receiver = self.dns_receiver(host, port)?;
            let answer = timeout(deadline, async {
                loop {
                    if let Some(answer) = receiver.borrow_and_update().clone() {
                        return answer;
                    }
                    receiver.changed().await.map_err(|_| "DNS resolver stopped".to_owned())?;
                }
            }).await.map_err(|_| HarnessError::Configuration(format!("comfyui.base_url='{}': DNS resolution timed out; ensure the local service name resolves", self.profile.base_url)))?;
            let addresses = answer.map_err(|error| HarnessError::Configuration(format!("comfyui.base_url='{}': cannot resolve local host ({error}); check service name and network", self.profile.base_url)))?;
            self.pinned_connection(addresses)
        }).await
    }

    fn dns_receiver(&self, host: &str, port: u16) -> Result<DnsReceiver> {
        let mut current = self
            .dns
            .lock()
            .map_err(|_| HarnessError::ComfyUi("DNS resolver lock poisoned".into()))?;
        if let Some(receiver) = current.as_ref() {
            let answer = receiver.borrow();
            if answer.as_ref().is_some_and(|result| result.is_ok())
                || (answer.is_none() && receiver.has_changed().is_ok())
            {
                return Ok(receiver.clone());
            }
        }
        let (sender, receiver) = watch::channel(None);
        let host = host.to_owned();
        std::thread::Builder::new()
            .name("genaimage-dns".into())
            .spawn(move || {
                let answer = (host.as_str(), port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect())
                    .map_err(|error| error.to_string());
                sender.send_replace(Some(answer));
            })
            .map_err(|error| {
                HarnessError::ComfyUi(format!("cannot start DNS resolver: {error}"))
            })?;
        *current = Some(receiver.clone());
        Ok(receiver)
    }

    fn pinned_connection(&self, addresses: Vec<SocketAddr>) -> Result<PinnedConnection> {
        self.profile.validate_addresses(&addresses)?;
        let host = self.base_url.host_str().expect("validated host");
        let http = Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|error| HarnessError::ComfyUi(format!("cannot build HTTP client: {error}")))?;
        Ok(PinnedConnection { http, addresses })
    }
}

#[async_trait]
impl ImageBackend for ComfyUiClient {
    fn name(&self) -> &str {
        "ComfyUI"
    }

    fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    fn with_timing(
        &self,
        request_timeout_seconds: u64,
        websocket_reconnect_seconds: u64,
    ) -> Option<Arc<dyn ImageBackend>> {
        // Cloning retains the in-progress DNS lookup, validated addresses and client pool.
        let mut updated = self.clone();
        updated.profile.request_timeout_seconds = request_timeout_seconds;
        updated.profile.websocket_reconnect_seconds = websocket_reconnect_seconds;
        Some(Arc::new(updated))
    }

    async fn health(&self) -> Result<BackendHealth> {
        // A health probe reports present availability without waiting for read retries.
        let value = self.get_json_with_attempts("system_stats", 1).await?;
        Ok(BackendHealth { system: value })
    }

    async fn object_info(&self) -> Result<Value> {
        self.get_json("object_info").await
    }

    async fn queue(&self) -> Result<QueueSnapshot> {
        let raw = self.get_json("queue").await?;
        Ok(QueueSnapshot {
            running: extract_queue_ids(raw.get("queue_running")),
            pending: extract_queue_ids(raw.get("queue_pending")),
            raw,
        })
    }

    async fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> Result<PromptResponse> {
        let response = self
            .connection()
            .await?
            .http
            .post(self.endpoint("prompt")?)
            .timeout(self.request_timeout())
            .json(&json!({
                "prompt": workflow,
                "client_id": client_id,
                "prompt_id": prompt_id
            }))
            .send()
            .await
            .map_err(|error| {
                HarnessError::ComfyUi(format!(
                    "submission to {} failed: {error}; prompt_id={prompt_id}",
                    self.base_url
                ))
            })?;
        parse_response::<PromptResponse>(response, "submit prompt").await
    }

    async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot> {
        let raw = self.get_json(&format!("history/{prompt_id}")).await?;
        Ok(parse_history(prompt_id, raw))
    }

    async fn delete_queued(&self, prompt_id: &str) -> Result<Value> {
        self.post_json("queue", &json!({ "delete": [prompt_id] }))
            .await
    }

    async fn interrupt(&self) -> Result<Value> {
        self.post_json("interrupt", &json!({})).await
    }

    async fn upload_image(&self, path: &Path) -> Result<UploadResponse> {
        let bytes = tokio::fs::read(path).await?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| HarnessError::InvalidPath {
                path: path.to_path_buf(),
                reason: "file name is not valid UTF-8".to_owned(),
            })?
            .to_owned();
        let mime = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream");
        let part = Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime)
            .map_err(|error| {
                HarnessError::ComfyUi(format!(
                    "cannot prepare upload '{}': {error}",
                    path.display()
                ))
            })?;
        let form = Form::new()
            .part("image", part)
            .text("overwrite", "false")
            .text("type", "input");
        let response = self
            .connection()
            .await?
            .http
            .post(self.endpoint("upload/image")?)
            .timeout(self.request_timeout())
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                HarnessError::ComfyUi(format!("image upload '{}' failed: {error}", path.display()))
            })?;
        #[derive(Deserialize)]
        struct RawUpload {
            name: String,
            #[serde(default)]
            subfolder: String,
            #[serde(default = "default_input_type", rename = "type")]
            kind: String,
        }
        let response = parse_response::<RawUpload>(response, "upload image").await?;
        Ok(UploadResponse {
            name: response.name,
            subfolder: response.subfolder,
            kind: response.kind,
        })
    }

    async fn download_output(&self, output: &OutputReference) -> Result<Vec<u8>> {
        let mut url = self.endpoint("view")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("filename", &output.filename);
            if !output.subfolder.is_empty() {
                query.append_pair("subfolder", &output.subfolder);
            }
            query.append_pair("type", &output.output_type);
        }
        self.get_bytes(url).await
    }

    async fn monitor_events(&self, client_id: &str) -> Result<EventMonitor> {
        let mut websocket_url = self.endpoint("ws")?;
        websocket_url
            .set_scheme(match websocket_url.scheme() {
                "https" => "wss",
                _ => "ws",
            })
            .map_err(|()| {
                HarnessError::Configuration("cannot construct ComfyUI WebSocket URL".to_owned())
            })?;
        websocket_url
            .query_pairs_mut()
            .append_pair("clientId", client_id);
        let reconnect_delay = Duration::from_secs(self.profile.websocket_reconnect_seconds);
        let addresses = self.connection().await?.addresses.clone();
        let (sender, receiver) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            loop {
                let connection = match tokio::net::TcpStream::connect(addresses.as_slice()).await {
                    Ok(stream) => {
                        client_async_tls_with_config(websocket_url.as_str(), stream, None, None)
                            .await
                    }
                    Err(error) => Err(error.into()),
                };
                match connection {
                    Ok((mut stream, _)) => {
                        while let Some(message) = stream.next().await {
                            let event = match message {
                                Ok(Message::Text(text)) => parse_event(&text),
                                Ok(Message::Close(frame)) => BackendEvent {
                                    event_type: "websocket_closed".to_owned(),
                                    prompt_id: None,
                                    node_id: None,
                                    value: None,
                                    maximum: None,
                                    message: frame.map(|frame| frame.reason.to_string()),
                                    raw: Value::Null,
                                },
                                Ok(_) => continue,
                                Err(error) => BackendEvent {
                                    event_type: "websocket_error".to_owned(),
                                    prompt_id: None,
                                    node_id: None,
                                    value: None,
                                    maximum: None,
                                    message: Some(error.to_string()),
                                    raw: Value::Null,
                                },
                            };
                            if sender.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let event = BackendEvent {
                            event_type: "websocket_error".to_owned(),
                            prompt_id: None,
                            node_id: None,
                            value: None,
                            maximum: None,
                            message: Some(format!("WebSocket connection failed: {error}")),
                            raw: Value::Null,
                        };
                        if sender.send(event).await.is_err() {
                            return;
                        }
                    }
                }
                sleep(reconnect_delay).await;
            }
        });
        Ok(EventMonitor::new(receiver, task))
    }
}

impl ComfyUiClient {
    async fn get_json(&self, path: &str) -> Result<Value> {
        self.get_json_with_attempts(path, MAX_SAFE_READ_ATTEMPTS)
            .await
    }

    async fn get_json_with_attempts(&self, path: &str, attempts: usize) -> Result<Value> {
        let url = self.endpoint(path)?;
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            let response = self
                .connection()
                .await?
                .http
                .get(url.clone())
                .timeout(self.request_timeout())
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    return response.json::<Value>().await.map_err(|error| {
                        HarnessError::ComfyUi(format!(
                            "ComfyUI GET {} returned invalid JSON: {error}",
                            url
                        ))
                    });
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    last_error = format!("HTTP {status}: {body}");
                    if !status.is_server_error() {
                        break;
                    }
                }
                Err(error) => last_error = error.to_string(),
            }
            if attempt < attempts {
                sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
        }
        Err(HarnessError::ComfyUi(format!(
            "ComfyUI GET {url} failed: {last_error}"
        )))
    }

    async fn get_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let mut last_error = String::new();
        for attempt in 1..=MAX_SAFE_READ_ATTEMPTS {
            let response = self
                .connection()
                .await?
                .http
                .get(url.clone())
                .timeout(self.request_timeout())
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    return response
                        .bytes()
                        .await
                        .map(|bytes| bytes.to_vec())
                        .map_err(|error| {
                            HarnessError::ComfyUi(format!(
                                "cannot read output bytes from {url}: {error}"
                            ))
                        });
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    last_error = format!("HTTP {status}: {body}");
                    if !status.is_server_error() {
                        break;
                    }
                }
                Err(error) => last_error = error.to_string(),
            }
            if attempt < MAX_SAFE_READ_ATTEMPTS {
                sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
        }
        Err(HarnessError::ComfyUi(format!(
            "ComfyUI download {url} failed after safe retries: {last_error}"
        )))
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let url = self.endpoint(path)?;
        let response = self
            .connection()
            .await?
            .http
            .post(url)
            .timeout(self.request_timeout())
            .json(body)
            .send()
            .await
            .map_err(|error| {
                HarnessError::ComfyUi(format!("ComfyUI POST {path} failed: {error}"))
            })?;
        parse_response(response, &format!("POST {path}")).await
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(HarnessError::Url)
    }
}

async fn parse_response<T>(response: reqwest::Response, operation: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let status = response.status();
    let url = response.url().clone();
    let body = response.text().await.map_err(|error| {
        HarnessError::ComfyUi(format!(
            "{operation} response from {url} could not be read: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(HarnessError::ComfyUi(format!(
            "{operation} at {url} returned {status}: {body}"
        )));
    }
    serde_json::from_str(&body).map_err(|error| {
        HarnessError::ComfyUi(format!(
            "{operation} at {url} returned invalid JSON: {error}; body={body}"
        ))
    })
}

fn default_input_type() -> String {
    "input".to_owned()
}

fn extract_queue_ids(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get(1).and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn extract_outputs(outputs: &Map<String, Value>) -> Vec<OutputReference> {
    let mut references = Vec::new();
    for (node_id, value) in outputs {
        let Some(output_object) = value.as_object() else {
            continue;
        };
        for (kind, values) in output_object {
            let Some(values) = values.as_array() else {
                continue;
            };
            for value in values {
                let Some(filename) = value
                    .get("filename")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    continue;
                };
                references.push(OutputReference {
                    node_id: node_id.clone(),
                    kind: kind.clone(),
                    filename,
                    subfolder: value
                        .get("subfolder")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    output_type: value
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("output")
                        .to_owned(),
                });
            }
        }
    }
    references
}

fn parse_history(prompt_id: &str, raw: Value) -> HistorySnapshot {
    let entry = raw.get(prompt_id);
    let status = entry.and_then(|entry| entry.get("status"));
    let explicit_status = status
        .and_then(|status| status.get("status_str"))
        .and_then(Value::as_str);
    let complete = status
        .and_then(|status| status.get("completed"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| entry.is_some_and(|entry| entry.get("outputs").is_some()));
    let messages = status
        .and_then(|status| status.get("messages"))
        .and_then(Value::as_array);
    let has_message = |kind: &str| {
        messages.is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get(0).and_then(Value::as_str) == Some(kind)
                    && message
                        .get(1)
                        .and_then(|data| data.get("prompt_id"))
                        .and_then(Value::as_str)
                        == Some(prompt_id)
            })
        })
    };
    let outcome = if has_message("execution_error") {
        HistoryOutcome::Failed
    } else if explicit_status == Some("execution_interrupted")
        || has_message("execution_interrupted")
    {
        // ComfyUI can report status_str="error" for an interrupted prompt.
        HistoryOutcome::Cancelled
    } else if explicit_status == Some("error") {
        HistoryOutcome::Failed
    } else if complete && matches!(explicit_status, None | Some("success")) {
        HistoryOutcome::Completed
    } else {
        HistoryOutcome::Unresolved
    };
    let outputs = entry
        .and_then(|entry| entry.get("outputs"))
        .and_then(Value::as_object)
        .map_or_else(Vec::new, extract_outputs);
    HistorySnapshot {
        prompt_id: prompt_id.to_owned(),
        outcome,
        outputs,
        raw,
    }
}

fn parse_event(text: &str) -> BackendEvent {
    let raw: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        json!({
            "type": "invalid_json",
            "data": { "error": error.to_string(), "raw": text }
        })
    });
    let data = raw.get("data").unwrap_or(&Value::Null);
    BackendEvent {
        event_type: raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        prompt_id: data
            .get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        node_id: data.get("node").and_then(Value::as_str).map(str::to_owned),
        value: data.get("value").and_then(Value::as_u64),
        maximum: data.get("max").and_then(Value::as_u64),
        message: data
            .get("exception_message")
            .or_else(|| data.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_event, parse_history, HistoryOutcome};
    use serde_json::json;

    #[test]
    fn history_distinguishes_interruption_from_failure_and_requires_the_target_prompt() {
        for (kind, id, expected) in [
            ("execution_interrupted", "target", HistoryOutcome::Cancelled),
            ("execution_interrupted", "different", HistoryOutcome::Failed),
            ("execution_error", "target", HistoryOutcome::Failed),
        ] {
            let raw = json!({"target": {"status": {
                "status_str": "error", "completed": false,
                "messages": [[kind, {"prompt_id": id}]],
            }}});
            assert_eq!(
                parse_history("absent", raw.clone()).outcome,
                HistoryOutcome::Unresolved
            );
            assert_eq!(parse_history("target", raw).outcome, expected);
        }
        let raw = json!({"target": {"status": {"status_str": "execution_interrupted", "completed": false}}});
        assert_eq!(
            parse_history("target", raw).outcome,
            HistoryOutcome::Cancelled
        );
    }

    #[test]
    fn parses_comfyui_progress_event() {
        let event = parse_event(
            r#"{"type":"progress","data":{"prompt_id":"p-1","node":"3","value":4,"max":20}}"#,
        );
        assert_eq!(event.event_type, "progress");
        assert_eq!(event.prompt_id.as_deref(), Some("p-1"));
        assert_eq!(event.node_id.as_deref(), Some("3"));
        assert_eq!(event.value, Some(4));
        assert_eq!(event.maximum, Some(20));
    }
    #[tokio::test]
    async fn timing_reload_reuses_validated_dns_answers_for_http_and_websocket() {
        use super::{ComfyUiClient, ComfyUiProfile, ImageBackend};
        use futures_util::SinkExt;
        use std::{
            net::SocketAddr,
            sync::{Arc, Mutex},
        };
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            time::{timeout, Duration},
        };
        use tokio_tungstenite::tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0; 4096];
                    let n = stream.peek(&mut buf).await.unwrap();
                    let request = String::from_utf8_lossy(&buf[..n]);
                    if request.starts_with("GET /ws?") {
                        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                        ws.send(Message::Text(
                            r#"{"type":"progress","data":{"prompt_id":"p","value":1}}"#.into(),
                        ))
                        .await
                        .unwrap();
                    } else {
                        assert!(request.starts_with("GET /system_stats"));
                        let _ = stream.read(&mut buf).await.unwrap();
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                    }
                });
            }
        });
        let public: SocketAddr = format!("8.8.8.8:{}", address.port()).parse().unwrap();
        let answers = Arc::new(Mutex::new(vec![address, public]));
        let profile = ComfyUiProfile {
            base_url: format!("http://pinned.invalid:{}", address.port()),
            allowed_hosts: vec!["pinned.invalid".into()],
            ..Default::default()
        };
        let mut client = ComfyUiClient::new(&profile).unwrap();
        client.test_addresses = Some(answers.clone());
        assert!(client
            .health()
            .await
            .unwrap_err()
            .to_string()
            .contains("public"));
        *answers.lock().unwrap() = vec![address];
        client.health().await.unwrap();
        *answers.lock().unwrap() = vec![public];
        let client = client.with_timing(1, 1).unwrap();
        client.health().await.unwrap();
        let mut events = client.monitor_events("client").await.unwrap();
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, "progress");
        assert_eq!(event.prompt_id.as_deref(), Some("p"));
        server.abort();
    }
    #[tokio::test]
    async fn stalled_dns_times_out_without_spawning_duplicate_resolvers() {
        use super::{ComfyUiClient, ComfyUiProfile, ImageBackend};
        use tokio::sync::watch;
        let profile = ComfyUiProfile {
            base_url: "http://waiting.invalid:8188".into(),
            allowed_hosts: vec!["waiting.invalid".into()],
            request_timeout_seconds: 1,
            ..Default::default()
        };
        let client = ComfyUiClient::new(&profile).unwrap();
        let (sender, receiver) = watch::channel(None);
        *client.dns.lock().unwrap() = Some(receiver);
        let error = client.health().await.unwrap_err().to_string();
        assert!(error.contains("DNS resolution timed out"), "{error}");
        assert_eq!(sender.receiver_count(), 1);
        let error = client.health().await.unwrap_err().to_string();
        assert!(error.contains("DNS resolution timed out"), "{error}");
        assert_eq!(sender.receiver_count(), 1);
    }
}
