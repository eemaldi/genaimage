//! Ollama-native model management (`/api/version`, `/api/tags`, `/api/pull`).
//!
//! The native API origin is derived from the configured OpenAI-compatible base
//! URL by removing a trailing `/v1`. A base URL that does not end in `/v1` has
//! no derivable native API, which is reported rather than guessed. Requests use
//! the provider's pinned, validated connection and API key. A pull is started
//! only by an explicit `pull` call.

use std::{fmt, time::Duration};

use futures_util::stream;
use genaimage_core::{
    async_trait,
    models::{
        InstalledModel, LlmModelManager, ModelListing, ModelSource, PullEvent, PullProgress,
        PullStream,
    },
    HarnessError, LlmProvider, Result,
};
use reqwest::{Response, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::{
    read_error_message, read_limited_body, redact, truncate, OpenAiCompatibleProvider,
    MAX_ERROR_MESSAGE,
};

const MAX_TAGS_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_VERSION_BODY_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_PULL_LINE_BYTES: usize = 64 * 1024;
const DEFAULT_PULL_OVERALL_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// Ollama can stay silent for minutes while it verifies a large layer, so a
/// pull tolerates far more silence than an ordinary completion request.
const DEFAULT_PULL_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const NOT_V1_REASON: &str = "The LLM base URL does not end in /v1, so the Ollama native API cannot be derived. Install models with your LLM server's own tooling.";
const NOT_OLLAMA_REASON: &str = "This endpoint does not answer Ollama's native API (/api/version), so it looks like another OpenAI-compatible server such as llama.cpp or LM Studio. Install models with that server's own tooling.";

pub struct OllamaModelManager {
    provider: OpenAiCompatibleProvider,
    native_base: Option<Url>,
    max_line_bytes: usize,
    overall_timeout: Duration,
    pull_idle_timeout: Duration,
}

impl fmt::Debug for OllamaModelManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OllamaModelManager")
            .field("native_base", &self.native_base)
            .field("max_line_bytes", &self.max_line_bytes)
            .field("overall_timeout", &self.overall_timeout)
            .field("pull_idle_timeout", &self.pull_idle_timeout)
            .finish_non_exhaustive()
    }
}

impl OllamaModelManager {
    pub fn new(provider: OpenAiCompatibleProvider) -> Self {
        let native_base = native_base(provider.base_url());
        Self {
            provider,
            native_base,
            max_line_bytes: DEFAULT_MAX_PULL_LINE_BYTES,
            overall_timeout: DEFAULT_PULL_OVERALL_TIMEOUT,
            pull_idle_timeout: DEFAULT_PULL_IDLE_TIMEOUT,
        }
    }

    /// Caps one streamed pull line and the whole pull.
    pub fn with_pull_limits(mut self, max_line_bytes: usize, overall_timeout: Duration) -> Self {
        self.max_line_bytes = max_line_bytes;
        self.overall_timeout = overall_timeout;
        self
    }

    /// Replaces the longest silence tolerated between pull chunks (default 10
    /// minutes; it does not use the provider's completion idle timeout).
    pub fn with_pull_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.pull_idle_timeout = idle_timeout;
        self
    }

    fn native(&self, path: &str) -> Option<Url> {
        self.native_base.as_ref()?.join(path).ok()
    }

    /// Whether the native API answers. An unreachable endpoint is an error, not
    /// "not Ollama".
    async fn is_ollama(&self) -> Result<bool> {
        let Some(url) = self.native("api/version") else {
            return Ok(false);
        };
        let response = self
            .provider
            .send(
                self.provider
                    .request(
                        &self.provider.connection().await?.http,
                        reqwest::Method::GET,
                        url,
                    )
                    .header(reqwest::header::ACCEPT, "application/json"),
            )
            .await?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let bytes = read_limited_body(
            response,
            self.provider.options.first_byte_timeout,
            MAX_VERSION_BODY_BYTES,
            "Ollama version",
            self.provider.options.api_key.as_deref(),
        )
        .await?;
        Ok(serde_json::from_slice::<Value>(&bytes)
            .is_ok_and(|value| value.get("version").is_some_and(Value::is_string)))
    }

    fn unsupported_reason(&self) -> &'static str {
        if self.native_base.is_none() {
            NOT_V1_REASON
        } else {
            NOT_OLLAMA_REASON
        }
    }

    async fn tags(&self) -> Result<Vec<InstalledModel>> {
        let url = self.native("api/tags").expect("native API checked");
        let response = self
            .provider
            .send(
                self.provider
                    .request(
                        &self.provider.connection().await?.http,
                        reqwest::Method::GET,
                        url,
                    )
                    .header(reqwest::header::ACCEPT, "application/json"),
            )
            .await?;
        if !response.status().is_success() {
            return Err(self.http_error(response, "Ollama model listing").await);
        }
        let bytes = read_limited_body(
            response,
            self.provider.options.first_byte_timeout,
            MAX_TAGS_BODY_BYTES,
            "Ollama model listing",
            self.provider.options.api_key.as_deref(),
        )
        .await?;
        let value = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
            self.provider.error(format!(
                "Ollama model listing returned invalid JSON: {error}"
            ))
        })?;
        let models = value
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.provider
                    .error("Ollama model listing JSON is missing the 'models' array")
            })?;
        models
            .iter()
            .map(|model| {
                let name = model
                    .get("name")
                    .or_else(|| model.get("model"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        self.provider
                            .error("Ollama model listing contains a model without a name")
                    })?;
                let text = |key: &str| model.get(key).and_then(Value::as_str).map(str::to_owned);
                Ok(InstalledModel {
                    name: name.to_owned(),
                    size_bytes: model.get("size").and_then(Value::as_u64),
                    digest: text("digest"),
                    modified_at: text("modified_at"),
                })
            })
            .collect()
    }

    async fn http_error(&self, response: Response, operation: &str) -> HarnessError {
        let status = response.status();
        let message = read_error_message(
            response,
            self.provider.options.idle_timeout,
            self.provider.options.api_key.as_deref(),
        )
        .await;
        self.provider
            .error(format!("{operation} returned HTTP {status}: {message}"))
    }
}

/// `http://host:11434/v1/` becomes `http://host:11434/`. `None` when the path
/// does not end in `/v1`.
fn native_base(base_url: &Url) -> Option<Url> {
    let root = base_url.path().strip_suffix("/v1/")?;
    let mut url = base_url.clone();
    url.set_path(&format!("{root}/"));
    Some(url)
}

#[async_trait]
impl LlmModelManager for OllamaModelManager {
    async fn list_installed(&self) -> Result<ModelListing> {
        if self.is_ollama().await? {
            return Ok(ModelListing {
                source: ModelSource::Ollama,
                pull_supported: true,
                pull_unsupported_reason: None,
                models: self.tags().await?,
            });
        }
        let models = self
            .provider
            .list_models()
            .await?
            .into_iter()
            .map(|model| InstalledModel {
                name: model.id,
                size_bytes: None,
                digest: None,
                modified_at: None,
            })
            .collect();
        Ok(ModelListing {
            source: ModelSource::OpenAi,
            pull_supported: false,
            pull_unsupported_reason: Some(self.unsupported_reason().to_owned()),
            models,
        })
    }

    async fn pull(&self, name: &str) -> Result<PullStream> {
        if !self.is_ollama().await? {
            return Err(HarnessError::UnsupportedOperation(
                self.unsupported_reason().to_owned(),
            ));
        }
        let url = self.native("api/pull").expect("native API checked");
        let response = self
            .provider
            .send(
                self.provider
                    .request(
                        &self.provider.connection().await?.http,
                        reqwest::Method::POST,
                        url,
                    )
                    .header(reqwest::header::ACCEPT, "application/x-ndjson")
                    .json(&json!({ "model": name, "name": name, "stream": true })),
            )
            .await?;
        if !response.status().is_success() {
            return Err(self.http_error(response, "Ollama pull").await);
        }
        Ok(pull_stream(
            response,
            PullLimits {
                idle: self.pull_idle_timeout,
                overall: self.overall_timeout,
                max_line: self.max_line_bytes,
                api_key: self.provider.options.api_key.clone(),
            },
        ))
    }
}

struct PullLimits {
    idle: Duration,
    overall: Duration,
    max_line: usize,
    api_key: Option<String>,
}

struct PullReader {
    response: Response,
    buffer: Vec<u8>,
    started: Instant,
    finished: bool,
    limits: PullLimits,
}

#[derive(Deserialize)]
struct PullLine {
    status: Option<String>,
    error: Option<String>,
    digest: Option<String>,
    total: Option<u64>,
    completed: Option<u64>,
}

/// Decodes the NDJSON body. The last item is always `Success` or `Failed`;
/// only an explicit `{"status":"success"}` line is success.
fn pull_stream(response: Response, limits: PullLimits) -> PullStream {
    let reader = PullReader {
        response,
        buffer: Vec::new(),
        started: Instant::now(),
        finished: false,
        limits,
    };
    Box::pin(stream::unfold(reader, |mut reader| async move {
        if reader.finished {
            return None;
        }
        let event = reader.next_event().await;
        reader.finished = !matches!(event, PullEvent::Progress(_));
        Some((event, reader))
    }))
}

impl PullReader {
    fn failed(&self, message: impl Into<String>) -> PullEvent {
        PullEvent::Failed {
            message: redact(self.limits.api_key.as_deref(), message.into()),
        }
    }

    async fn next_event(&mut self) -> PullEvent {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = self.buffer.drain(..=end).collect::<Vec<_>>();
                if line.len() > self.limits.max_line + 1 {
                    return self.too_long();
                }
                if let Some(event) = self.parse(&line) {
                    return event;
                }
                continue;
            }
            if self.buffer.len() > self.limits.max_line {
                return self.too_long();
            }
            let Some(remaining) = self
                .limits
                .overall
                .checked_sub(self.started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                return self.failed(format!(
                    "The pull exceeded the {} second limit and was stopped.",
                    self.limits.overall.as_secs()
                ));
            };
            match tokio::time::timeout(self.limits.idle.min(remaining), self.response.chunk()).await
            {
                Err(_) if remaining <= self.limits.idle => {
                    return self.failed(format!(
                        "The pull exceeded the {} second limit and was stopped.",
                        self.limits.overall.as_secs()
                    ))
                }
                Err(_) => {
                    return self.failed(format!(
                        "Ollama sent no data for {} seconds; the pull was stopped.",
                        self.limits.idle.as_secs_f64()
                    ))
                }
                Ok(Err(error)) => return self.failed(format!("The pull stream broke: {error}")),
                Ok(Ok(Some(chunk))) => self.buffer.extend_from_slice(&chunk),
                Ok(Ok(None)) => {
                    let rest = std::mem::take(&mut self.buffer);
                    if let Some(event @ (PullEvent::Success | PullEvent::Failed { .. })) =
                        self.parse(&rest)
                    {
                        return event;
                    }
                    return self.failed(
                        "The pull stream ended without Ollama reporting success; the model may be incomplete.",
                    );
                }
            }
        }
    }

    fn too_long(&self) -> PullEvent {
        self.failed(format!(
            "Ollama sent a pull line longer than {} bytes; the pull was stopped.",
            self.limits.max_line
        ))
    }

    /// `None` for a blank line.
    fn parse(&self, line: &[u8]) -> Option<PullEvent> {
        let line = line.trim_ascii();
        if line.is_empty() {
            return None;
        }
        let Ok(parsed) = serde_json::from_slice::<PullLine>(line) else {
            return Some(self.failed("Ollama sent a pull line that is not valid progress JSON."));
        };
        if let Some(error) = parsed.error {
            return Some(self.failed(truncate(error.trim(), MAX_ERROR_MESSAGE)));
        }
        let Some(status) = parsed.status else {
            return Some(self.failed("Ollama sent a pull line without a status."));
        };
        if status == "success" {
            return Some(PullEvent::Success);
        }
        Some(PullEvent::Progress(PullProgress {
            status: redact(
                self.limits.api_key.as_deref(),
                truncate(&status, MAX_ERROR_MESSAGE),
            ),
            digest: parsed.digest,
            total_bytes: parsed.total,
            completed_bytes: parsed.completed,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_base_strips_only_a_trailing_v1() {
        let derive = |url: &str| native_base(&Url::parse(url).unwrap()).map(String::from);
        assert_eq!(
            derive("http://ollama:11434/v1/"),
            Some("http://ollama:11434/".into())
        );
        assert_eq!(
            derive("http://127.0.0.1:11434/proxy/v1/"),
            Some("http://127.0.0.1:11434/proxy/".into())
        );
        assert_eq!(derive("http://127.0.0.1:8080/"), None);
        assert_eq!(derive("http://127.0.0.1:8080/v2/"), None);
        assert_eq!(derive("http://127.0.0.1:8080/xv1/"), None);
    }
}
