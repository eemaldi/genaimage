use std::{
    collections::BTreeSet,
    fmt,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

use super::{invalid, validate_endpoint};
use crate::{Event, Result};

pub const DEFAULT_WEBHOOK_TIMEOUT_SECONDS: u64 = 5;
pub const DEFAULT_WEBHOOK_MAX_ATTEMPTS: u32 = 5;
const MAX_WEBHOOK_TIMEOUT_SECONDS: u64 = 120;
const MAX_WEBHOOK_ATTEMPTS: u32 = 20;
const MAX_PATH_BYTES: usize = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SinkKind {
    Jsonl,
    Webhook,
}

impl SinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Webhook => "webhook",
        }
    }
}

/// A webhook signing secret. It is read from an environment variable, never
/// serialised and never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct SinkSecret(String);

impl SinkSecret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SinkSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// One `[[sinks]]` table. Read-only in the settings API.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SinkConfig {
    pub name: String,
    pub kind: SinkKind,
    /// JSONL only: a relative path below `<data dir>/sinks`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Webhook only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// Webhook only: the environment variable holding the HMAC key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// Event `type` names to deliver; omitted means every event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(skip)]
    pub secret: Option<SinkSecret>,
}

impl SinkConfig {
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
            .unwrap_or(DEFAULT_WEBHOOK_TIMEOUT_SECONDS)
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts.unwrap_or(DEFAULT_WEBHOOK_MAX_ATTEMPTS)
    }

    /// True when the sink wants events of this kind.
    pub fn wants(&self, event: &Event) -> bool {
        match &self.events {
            Some(kinds) => kinds.iter().any(|kind| kind == event.kind()),
            None => true,
        }
    }
}

pub(super) fn resolve(
    mut sinks: Vec<SinkConfig>,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<SinkConfig>> {
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for sink in &mut sinks {
        validate_name(&sink.name)?;
        if !names.insert(sink.name.clone()) {
            return Err(invalid(
                "sinks.name",
                &sink.name,
                "give every sink a unique name",
            ));
        }
        let key = |field: &str| format!("sinks[{}].{field}", sink.name);
        if let Some(events) = &sink.events {
            if events.is_empty() {
                return Err(invalid(
                    &key("events"),
                    "[]",
                    "list at least one event type or omit the key to receive every event",
                ));
            }
            if let Some(unknown) = events
                .iter()
                .find(|kind| !Event::KINDS.contains(&kind.as_str()))
            {
                return Err(invalid(
                    &key("events"),
                    unknown,
                    &format!("use event types from: {}", Event::KINDS.join(", ")),
                ));
            }
        }
        match sink.kind {
            SinkKind::Jsonl => {
                for (field, present) in [
                    ("url", sink.url.is_some()),
                    ("allowed_hosts", !sink.allowed_hosts.is_empty()),
                    ("secret_env", sink.secret_env.is_some()),
                    ("timeout_seconds", sink.timeout_seconds.is_some()),
                    ("max_attempts", sink.max_attempts.is_some()),
                ] {
                    if present {
                        return Err(invalid(
                            &key(field),
                            "<set>",
                            "remove the key: it applies to webhook sinks only",
                        ));
                    }
                }
                let path = sink.path.as_deref().ok_or_else(|| {
                    invalid(&key("path"), "", "set a relative path for a jsonl sink")
                })?;
                validate_relative_path(&key("path"), path)?;
                if path.ends_with(crate::sinks::DEAD_LETTER_SUFFIX) {
                    return Err(invalid(
                        &key("path"),
                        path,
                        "that suffix is reserved for webhook dead-letter logs",
                    ));
                }
                if !paths.insert(Path::new(path).components().collect::<std::path::PathBuf>()) {
                    return Err(invalid(
                        &key("path"),
                        path,
                        "give every jsonl sink its own file",
                    ));
                }
            }
            SinkKind::Webhook => {
                if sink.path.is_some() {
                    return Err(invalid(
                        &key("path"),
                        "<set>",
                        "remove the key: it applies to jsonl sinks only",
                    ));
                }
                let url = sink.url.as_deref().ok_or_else(|| {
                    invalid(
                        &key("url"),
                        "",
                        "set a local http(s) URL for a webhook sink",
                    )
                })?;
                validate_endpoint(
                    &format!("sinks[{}].url", sink.name),
                    url,
                    &sink.allowed_hosts,
                )?;
                let timeout = sink.timeout_seconds();
                if !(1..=MAX_WEBHOOK_TIMEOUT_SECONDS).contains(&timeout) {
                    return Err(invalid(
                        &key("timeout_seconds"),
                        &timeout.to_string(),
                        &format!("use 1 to {MAX_WEBHOOK_TIMEOUT_SECONDS} seconds"),
                    ));
                }
                let attempts = sink.max_attempts();
                if !(1..=MAX_WEBHOOK_ATTEMPTS).contains(&attempts) {
                    return Err(invalid(
                        &key("max_attempts"),
                        &attempts.to_string(),
                        &format!("use 1 to {MAX_WEBHOOK_ATTEMPTS} attempts"),
                    ));
                }
                if let Some(variable) = &sink.secret_env {
                    validate_env_name(&key("secret_env"), variable)?;
                    let value = env(variable).filter(|value| !value.is_empty()).ok_or_else(|| {
                        invalid(
                            &key("secret_env"),
                            variable,
                            "the named environment variable is not set or is empty; export it before starting the server or remove secret_env",
                        )
                    })?;
                    sink.secret = Some(SinkSecret::new(value));
                }
            }
        }
    }
    Ok(sinks)
}

fn validate_name(name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    let valid = name.len() <= 64
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        });
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "sinks.name",
            name,
            "use 1 to 64 characters from [a-z0-9_-], starting with a letter or digit",
        ))
    }
}

fn validate_env_name(key: &str, name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(invalid(
            key,
            name,
            "name an environment variable using letters, digits and underscores",
        ))
    }
}

fn validate_relative_path(key: &str, path: &str) -> Result<()> {
    let candidate = Path::new(path);
    let plain = !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && candidate
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if plain {
        Ok(())
    } else {
        Err(invalid(
            key,
            path,
            "use a relative path without '..' or a root; it is created below <data dir>/sinks",
        ))
    }
}
