use std::collections::BTreeSet;
use std::{path::Path, time::Duration};

use futures_util::StreamExt;
use reqwest::{
    multipart::{Form, Part},
    Client, Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{config::ComfyUiProfile, HarnessError, Result};

const MAX_SAFE_READ_ATTEMPTS: usize = 3;

#[derive(Clone, Debug)]
pub struct ComfyUiClient {
    http: Client,
    base_url: Url,
    websocket_reconnect: Duration,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComfyUiHealth {
    pub system: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PromptResponse {
    pub prompt_id: String,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub node_errors: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueueSnapshot {
    pub running: Vec<String>,
    pub pending: Vec<String>,
    pub raw: Value,
}

impl QueueSnapshot {
    pub fn contains(&self, prompt_id: &str) -> bool {
        self.running.contains(&prompt_id.to_owned()) || self.pending.contains(&prompt_id.to_owned())
    }

    pub fn is_running(&self, prompt_id: &str) -> bool {
        self.running.contains(&prompt_id.to_owned())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadResponse {
    pub name: String,
    pub subfolder: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputReference {
    pub node_id: String,
    pub kind: String,
    pub filename: String,
    pub subfolder: String,
    pub output_type: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistorySnapshot {
    pub prompt_id: String,
    pub complete: bool,
    pub success: bool,
    pub outputs: Vec<OutputReference>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComfyUiEvent {
    pub event_type: String,
    pub prompt_id: Option<String>,
    pub node_id: Option<String>,
    pub value: Option<u64>,
    pub maximum: Option<u64>,
    pub message: Option<String>,
    pub raw: Value,
}

pub struct EventMonitor {
    pub receiver: mpsc::Receiver<ComfyUiEvent>,
    task: JoinHandle<()>,
}

impl EventMonitor {
    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for EventMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ComfyUiClient {
    pub fn new(profile: &ComfyUiProfile) -> Result<Self> {
        let base_url = Url::parse(&profile.base_url)?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(HarnessError::Configuration(format!(
                "ComfyUI profile URL must use http or https, got '{}'",
                base_url.scheme()
            )));
        }
        if base_url.host_str().is_none() {
            return Err(HarnessError::Configuration(
                "ComfyUI profile URL is missing a host".to_owned(),
            ));
        }
        if !is_loopback_host(base_url.host_str().unwrap_or_default()) {
            return Err(HarnessError::Configuration(format!(
                "this release supports one local ComfyUI host only; '{}' is not loopback",
                base_url.host_str().unwrap_or_default()
            )));
        }
        let mut base_url = base_url;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(profile.request_timeout_seconds))
            .build()
            .map_err(|error| HarnessError::ComfyUi(format!("cannot build HTTP client: {error}")))?;
        Ok(Self {
            http,
            base_url,
            websocket_reconnect: Duration::from_secs(profile.websocket_reconnect_seconds),
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub async fn health(&self) -> Result<ComfyUiHealth> {
        let value = self.get_json("system_stats").await?;
        Ok(ComfyUiHealth { system: value })
    }

    pub async fn object_info(&self, class_type: Option<&str>) -> Result<Value> {
        let path = class_type.map_or_else(
            || "object_info".to_owned(),
            |value| format!("object_info/{value}"),
        );
        self.get_json(&path).await
    }

    pub async fn validate_workflow_nodes(&self, workflow: &Value) -> Result<()> {
        let nodes = workflow.as_object().ok_or_else(|| {
            HarnessError::Workflow("API workflow root must be an object".to_owned())
        })?;
        let class_types: BTreeSet<String> = nodes
            .values()
            .filter_map(|node| node.get("class_type").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let missing_class_types: Vec<&str> = nodes
            .iter()
            .filter_map(|(node_id, node)| {
                node.get("class_type")
                    .and_then(Value::as_str)
                    .is_none()
                    .then_some(node_id.as_str())
            })
            .collect();
        if !missing_class_types.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "nodes {} are missing class_type",
                missing_class_types.join(", ")
            )));
        }
        let available = self.object_info(None).await?;
        let available = available.as_object().ok_or_else(|| {
            HarnessError::ComfyUi(
                "ComfyUI /object_info did not return an object; cannot validate nodes".to_owned(),
            )
        })?;
        let unavailable: Vec<&str> = class_types
            .iter()
            .filter(|class_type| !available.contains_key(class_type.as_str()))
            .map(String::as_str)
            .collect();
        if !unavailable.is_empty() {
            return Err(HarnessError::Workflow(format!(
                "ComfyUI does not provide required node types: {}",
                unavailable.join(", ")
            )));
        }
        Ok(())
    }

    pub async fn queue(&self) -> Result<QueueSnapshot> {
        let raw = self.get_json("queue").await?;
        Ok(QueueSnapshot {
            running: extract_queue_ids(raw.get("queue_running")),
            pending: extract_queue_ids(raw.get("queue_pending")),
            raw,
        })
    }

    pub async fn submit(
        &self,
        workflow: &Value,
        client_id: &str,
        prompt_id: &str,
    ) -> Result<PromptResponse> {
        let response = self
            .http
            .post(self.endpoint("prompt")?)
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

    pub async fn history(&self, prompt_id: &str) -> Result<HistorySnapshot> {
        let raw = self.get_json(&format!("history/{prompt_id}")).await?;
        let entry = raw.get(prompt_id).cloned();
        let Some(entry) = entry else {
            return Ok(HistorySnapshot {
                prompt_id: prompt_id.to_owned(),
                complete: false,
                success: false,
                outputs: Vec::new(),
                raw,
            });
        };
        let status = entry.get("status");
        let complete = status
            .and_then(|value| value.get("completed"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| entry.get("outputs").is_some());
        let explicit_status = status
            .and_then(|value| value.get("status_str"))
            .and_then(Value::as_str);
        let success = explicit_status.map_or_else(
            || {
                complete
                    && !status
                        .and_then(|value| value.get("messages"))
                        .and_then(Value::as_array)
                        .is_some_and(|messages| messages.iter().any(is_execution_error))
            },
            |value| value == "success",
        );
        let outputs = entry
            .get("outputs")
            .and_then(Value::as_object)
            .map_or_else(Vec::new, extract_outputs);

        Ok(HistorySnapshot {
            prompt_id: prompt_id.to_owned(),
            complete,
            success,
            outputs,
            raw,
        })
    }

    pub async fn delete_queued(&self, prompt_id: &str) -> Result<Value> {
        self.post_json("queue", &json!({ "delete": [prompt_id] }))
            .await
    }

    pub async fn interrupt(&self) -> Result<Value> {
        self.post_json("interrupt", &json!({})).await
    }

    pub async fn upload_image(&self, path: &Path) -> Result<UploadResponse> {
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
            .http
            .post(self.endpoint("upload/image")?)
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

    pub async fn download_output(&self, output: &OutputReference) -> Result<Vec<u8>> {
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

    pub fn monitor_events(&self, client_id: &str) -> Result<EventMonitor> {
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
        let reconnect_delay = self.websocket_reconnect;
        let (sender, receiver) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            loop {
                match connect_async(websocket_url.as_str()).await {
                    Ok((mut stream, _)) => {
                        while let Some(message) = stream.next().await {
                            let event = match message {
                                Ok(Message::Text(text)) => parse_event(&text),
                                Ok(Message::Close(frame)) => ComfyUiEvent {
                                    event_type: "websocket_closed".to_owned(),
                                    prompt_id: None,
                                    node_id: None,
                                    value: None,
                                    maximum: None,
                                    message: frame.map(|frame| frame.reason.to_string()),
                                    raw: Value::Null,
                                },
                                Ok(_) => continue,
                                Err(error) => ComfyUiEvent {
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
                        let event = ComfyUiEvent {
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
        Ok(EventMonitor { receiver, task })
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let url = self.endpoint(path)?;
        let mut last_error = String::new();
        for attempt in 1..=MAX_SAFE_READ_ATTEMPTS {
            let response = self.http.get(url.clone()).send().await;
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
            if attempt < MAX_SAFE_READ_ATTEMPTS {
                sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
        }
        Err(HarnessError::ComfyUi(format!(
            "ComfyUI GET {url} failed after safe retries: {last_error}"
        )))
    }

    async fn get_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let mut last_error = String::new();
        for attempt in 1..=MAX_SAFE_READ_ATTEMPTS {
            let response = self.http.get(url.clone()).send().await;
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
            .http
            .post(url)
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

fn is_execution_error(message: &Value) -> bool {
    message
        .as_array()
        .and_then(|message| message.first())
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "execution_error")
}

fn parse_event(text: &str) -> ComfyUiEvent {
    let raw: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        json!({
            "type": "invalid_json",
            "data": { "error": error.to_string(), "raw": text }
        })
    });
    let data = raw.get("data").unwrap_or(&Value::Null);
    ComfyUiEvent {
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

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod tests {
    use super::{is_loopback_host, parse_event};

    #[test]
    fn accepts_only_supported_local_hosts() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("192.168.1.10"));
        assert!(!is_loopback_host("example.com"));
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
}
