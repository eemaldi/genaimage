use std::{path::Path, time::Duration};

use futures_util::StreamExt;
use reqwest::{
    multipart::{Form, Part},
    redirect::Policy,
    Client, Url,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::{sync::mpsc, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};

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

#[derive(Clone, Debug)]
pub struct ComfyUiClient {
    http: Client,
    base_url: Url,
    websocket_reconnect: Duration,
}

impl ComfyUiClient {
    pub fn new(profile: &ComfyUiProfile) -> Result<Self> {
        let mut base_url = profile.validate_base_url()?;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let submit_url = base_url.join("prompt")?;
        let default_redirects = Policy::default();
        let http = Client::builder()
            // Only explicit safe reads may retry. Never replay a prompt POST,
            // including when a redirect preserves the request method and body.
            .retry(reqwest::retry::never())
            .redirect(Policy::custom(move |attempt| {
                if attempt.previous().first() == Some(&submit_url) {
                    attempt.stop()
                } else {
                    default_redirects.redirect(attempt)
                }
            }))
            .timeout(Duration::from_secs(profile.request_timeout_seconds))
            .build()
            .map_err(|error| HarnessError::ComfyUi(format!("cannot build HTTP client: {error}")))?;
        Ok(Self {
            http,
            base_url,
            websocket_reconnect: Duration::from_secs(profile.websocket_reconnect_seconds),
        })
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

    async fn health(&self) -> Result<BackendHealth> {
        let value = self.get_json("system_stats").await?;
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
        let reconnect_delay = self.websocket_reconnect;
        let (sender, receiver) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            loop {
                match connect_async(websocket_url.as_str()).await {
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
}
