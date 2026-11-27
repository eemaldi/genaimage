//! Webhook event sink for local endpoints.
//!
//! Deliveries are at-least-once and sequential, so receivers must de-duplicate
//! on the `Idempotency-Key` header. Each attempt resolves the host again and
//! validates every answer against the endpoint policy before connecting.

use std::{fmt, path::PathBuf, sync::Arc, time::Duration};

use chrono::Utc;
use genaimage_core::{
    async_trait,
    config::{validate_endpoint, validate_endpoint_addresses, SinkConfig, SinkSecret},
    sinks::{append_line, hmac_sha256_hex, redact_and_bound, SinkStatus, MAX_LINE_BYTES},
    EventEnvelope, EventSink, HarnessError, Result, SinkNotification,
};
use reqwest::{redirect, Client, StatusCode};
use serde_json::json;
use url::Url;

pub const SIGNATURE_HEADER: &str = "X-GenAImage-Signature";
pub const TIMESTAMP_HEADER: &str = "X-GenAImage-Timestamp";
pub const EVENT_HEADER: &str = "X-GenAImage-Event";
pub const ATTEMPT_HEADER: &str = "X-GenAImage-Attempt";
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(250);

pub struct WebhookOptions {
    pub name: String,
    pub url: String,
    pub allowed_hosts: Vec<String>,
    pub secret: Option<SinkSecret>,
    pub timeout: Duration,
    pub max_attempts: u32,
    pub dead_letter_path: PathBuf,
    /// First retry delay; it doubles per attempt up to 30 s, with jitter.
    pub backoff_base: Duration,
}

impl WebhookOptions {
    pub fn from_config(config: &SinkConfig, dead_letter_path: PathBuf) -> Result<Self> {
        let url = config.url.clone().ok_or_else(|| {
            HarnessError::Configuration(format!("webhook sink '{}' has no url", config.name))
        })?;
        Ok(Self {
            name: config.name.clone(),
            url,
            allowed_hosts: config.allowed_hosts.clone(),
            secret: config.secret.clone(),
            timeout: Duration::from_secs(config.timeout_seconds()),
            max_attempts: config.max_attempts(),
            dead_letter_path,
            backoff_base: DEFAULT_BACKOFF_BASE,
        })
    }
}

impl fmt::Debug for WebhookOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookOptions")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

pub struct WebhookSink {
    url: Url,
    options: WebhookOptions,
    status: Arc<SinkStatus>,
}

impl fmt::Debug for WebhookSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookSink")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

struct Failure {
    retryable: bool,
    message: String,
}

impl WebhookSink {
    pub fn new(options: WebhookOptions, status: Arc<SinkStatus>) -> Result<Self> {
        let url = validate_endpoint(
            &format!("sinks[{}].url", options.name),
            &options.url,
            &options.allowed_hosts,
        )?;
        if options.max_attempts == 0 || options.timeout.is_zero() {
            return Err(HarnessError::Configuration(format!(
                "webhook sink '{}' needs a positive timeout and at least one attempt",
                options.name
            )));
        }
        Ok(Self {
            url,
            options,
            status,
        })
    }

    fn secret(&self) -> Option<&str> {
        self.options.secret.as_ref().map(SinkSecret::expose)
    }

    fn failure(&self, retryable: bool, message: impl AsRef<str>) -> Failure {
        Failure {
            retryable,
            message: redact_and_bound(message.as_ref(), self.secret()),
        }
    }

    async fn attempt(
        &self,
        envelope: &EventEnvelope,
        body: &[u8],
        attempt: u32,
    ) -> std::result::Result<(), Failure> {
        let host = self.url.host_str().expect("validated host");
        let port = self.url.port_or_known_default().expect("validated scheme");
        let addresses: Vec<_> = tokio::time::timeout(
            self.options.timeout,
            tokio::net::lookup_host((host.trim_matches(['[', ']']), port)),
        )
        .await
        .map_err(|_| self.failure(true, format!("resolving {host} timed out")))?
        .map_err(|error| self.failure(true, format!("cannot resolve {host}: {error}")))?
        .collect();
        validate_endpoint_addresses(
            &format!("sinks[{}].url", self.options.name),
            &self.options.url,
            &self.options.allowed_hosts,
            &addresses,
        )
        .map_err(|error| self.failure(false, error.to_string()))?;
        let client = Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(self.options.timeout)
            .timeout(self.options.timeout)
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|error| self.failure(false, format!("cannot build HTTP client: {error}")))?;
        let key = format!("{}:{}", envelope.bus_id, envelope.id);
        let mut request = client
            .post(self.url.clone())
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", &key)
            .header(EVENT_HEADER, envelope.event.kind())
            .header(ATTEMPT_HEADER, attempt.to_string())
            .body(body.to_vec());
        if let Some(secret) = self.secret() {
            let timestamp = Utc::now().timestamp().to_string();
            let mut signed = format!("{timestamp}.").into_bytes();
            signed.extend_from_slice(body);
            request = request.header(TIMESTAMP_HEADER, &timestamp).header(
                SIGNATURE_HEADER,
                format!("sha256={}", hmac_sha256_hex(secret.as_bytes(), &signed)),
            );
        }
        let response = request.send().await.map_err(|error| {
            self.failure(true, format!("request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else if status.is_redirection() {
            Err(self.failure(false, format!("HTTP {status}: redirects are not followed")))
        } else if status.is_server_error()
            || status == StatusCode::TOO_MANY_REQUESTS
            || status == StatusCode::REQUEST_TIMEOUT
        {
            Err(self.failure(true, format!("HTTP {status}")))
        } else {
            Err(self.failure(
                false,
                format!("HTTP {status}: the receiver rejected the event"),
            ))
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let ceiling = self
            .options
            .backoff_base
            .saturating_mul(1_u32 << (attempt - 1).min(16))
            .min(MAX_BACKOFF);
        let half = ceiling / 2;
        let jitter_range = u64::try_from(half.as_millis()).unwrap_or(u64::MAX) + 1;
        let random = u64::try_from(uuid::Uuid::new_v4().as_u128() % u128::from(jitter_range))
            .unwrap_or_default();
        half + Duration::from_millis(random)
    }

    async fn dead_letter(
        &self,
        envelope: &EventEnvelope,
        attempts: u32,
        permanent: bool,
        error: &str,
    ) -> Result<()> {
        let mut record = json!({
            "sink": self.options.name,
            "failed_at": Utc::now(),
            "attempts": attempts,
            "permanent": permanent,
            "last_error": error,
            "event": envelope,
        });
        let mut line = record.to_string();
        if line.len() >= MAX_LINE_BYTES {
            record["event"] = json!({
                "omitted": "over the line limit",
                "bus_id": envelope.bus_id,
                "id": envelope.id,
                "type": envelope.event.kind(),
            });
            line = record.to_string();
        }
        append_line(&self.options.dead_letter_path, line)
            .await
            .map_err(|error| {
                HarnessError::Configuration(format!(
                    "cannot write dead-letter log '{}': {error}",
                    self.options.dead_letter_path.display()
                ))
            })?;
        self.status.record_dead_letter();
        Ok(())
    }

    fn undelivered(&self, id: u64, attempts: u32, error: &str) -> HarnessError {
        HarnessError::Backend {
            backend: format!("webhook '{}'", self.options.name),
            message: format!("event {id} not delivered after {attempts} attempt(s): {error}"),
        }
    }
}

#[async_trait]
impl EventSink for WebhookSink {
    async fn handle(&self, notification: &SinkNotification) -> Result<()> {
        // A gap is recorded in the status and the doctor check. Receivers can
        // also see it as a jump in `id` (ids increase for the life of the bus).
        let SinkNotification::Event(envelope) = notification else {
            return Ok(());
        };
        let body = serde_json::to_vec(envelope.as_ref())
            .map_err(|error| HarnessError::Configuration(error.to_string()))?;
        if body.len() >= MAX_LINE_BYTES {
            let message = format!("event body of {} bytes is over the limit", body.len());
            self.dead_letter(envelope, 0, true, &message).await?;
            return Err(self.undelivered(envelope.id, 0, &message));
        }
        let mut attempt = 1;
        loop {
            let failure = match self.attempt(envelope, &body, attempt).await {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            if failure.retryable && attempt < self.options.max_attempts {
                self.status.record_retry(&failure.message);
                tokio::time::sleep(self.backoff(attempt)).await;
                attempt += 1;
                continue;
            }
            self.dead_letter(envelope, attempt, !failure.retryable, &failure.message)
                .await?;
            return Err(self.undelivered(envelope.id, attempt, &failure.message));
        }
    }
}
