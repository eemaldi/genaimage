use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, RwLock,
};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::config::{LlmCapabilityOverride, LlmConfig};
use crate::extensions::{ChatContent, ChatMessage, ChatRequest, LlmModel};
use crate::{HarnessError, LlmProvider, Result};

pub const LLM_PROVIDER: &str = "openai-compatible";
const MODEL_CACHE_TTL: Duration = Duration::from_secs(30);
const MODEL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmRole {
    Text,
    Vision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmCapability {
    Tools,
    Vision,
}

impl LlmCapability {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Vision => "vision",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AssistantUnavailable {
    #[error("assistant {role:?} role is not configured: {reason}")]
    NotConfigured { role: LlmRole, reason: String },
    #[error("LLM model probe is unreachable: {reason}")]
    Unreachable { reason: String },
    #[error("LLM model '{model}' was not found")]
    ModelNotFound { model: String },
    #[error("LLM model '{model}' lacks the required {capability:?} capability")]
    CapabilityUnsupported {
        model: String,
        capability: LlmCapability,
    },
}

#[derive(Clone)]
pub struct LlmRoleBinding {
    provider: Arc<dyn LlmProvider>,
    model: String,
    supports_tools: bool,
    supports_vision: bool,
}

impl std::fmt::Debug for LlmRoleBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmRoleBinding")
            .field("model", &self.model)
            .field("supports_tools", &self.supports_tools)
            .field("supports_vision", &self.supports_vision)
            .finish_non_exhaustive()
    }
}

impl LlmRoleBinding {
    pub fn provider(&self) -> Arc<dyn LlmProvider> {
        self.provider.clone()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub const fn supports_tools(&self) -> bool {
        self.supports_tools
    }

    pub const fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    pub fn check_request(&self, request: &ChatRequest) -> Result<()> {
        if request.model != self.model {
            return Err(HarnessError::Configuration(format!(
                "chat request selects model '{}', but this role is pinned to model '{}'",
                request.model, self.model
            )));
        }
        if !request.tools.is_empty() && !self.supports_tools {
            return Err(HarnessError::Configuration(format!(
                "model '{}' lacks the required tools capability",
                self.model
            )));
        }
        let has_images = request.messages.iter().any(|message| {
            matches!(message, ChatMessage::User(content) if content.iter().any(|part| matches!(part, ChatContent::Image { .. })))
        });
        if has_images && !self.supports_vision {
            return Err(HarnessError::Configuration(format!(
                "model '{}' lacks the required vision capability",
                self.model
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LlmRoleStatus {
    Ok,
    NotConfigured,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmRoleHealth {
    pub status: LlmRoleStatus,
    pub model: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmHealthSnapshot {
    pub status: LlmRoleStatus,
    pub reason: Option<String>,
    pub text: Option<LlmRoleHealth>,
    pub vision: Option<LlmRoleHealth>,
    pub models: Vec<LlmModelSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmModelSnapshot {
    pub id: String,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

#[derive(Clone, Debug)]
enum ProbeCache {
    Pending,
    Ready { models: Vec<LlmModel>, at: Instant },
    Failed { reason: String, at: Instant },
}

impl ProbeCache {
    fn fresh(&self, now: Instant) -> bool {
        match self {
            Self::Pending => false,
            Self::Ready { at, .. } | Self::Failed { at, .. } => {
                now.duration_since(*at) < MODEL_CACHE_TTL
            }
        }
    }
}

#[derive(Clone)]
pub struct LlmRoles {
    provider: Option<Arc<dyn LlmProvider>>,
    runtime_config: Arc<LlmConfig>,
    base_configured: bool,
    text_model: String,
    vision_model: String,
    capability_overrides: std::collections::BTreeMap<String, LlmCapabilityOverride>,
    cache: Arc<RwLock<ProbeCache>>,
    probe: Arc<Mutex<()>>,
    completed_probes: Arc<AtomicUsize>,
    probe_timeout: Duration,
    background_refreshing: Arc<AtomicBool>,
}

impl std::fmt::Debug for LlmRoles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmRoles")
            .field("configured", &self.base_configured)
            .field("text_model", &self.text_model)
            .field("vision_model", &self.vision_model)
            .field("capability_overrides", &self.capability_overrides)
            .field("probe_timeout", &self.probe_timeout)
            .finish_non_exhaustive()
    }
}

impl LlmRoles {
    pub fn new(config: &LlmConfig, provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            provider,
            runtime_config: Arc::new(config.clone()),
            base_configured: !config.base_url.is_empty(),
            text_model: config.text_model.clone(),
            vision_model: config.vision_model.clone(),
            capability_overrides: config.capabilities.clone(),
            cache: Arc::new(RwLock::new(ProbeCache::Pending)),
            probe: Arc::new(Mutex::new(())),
            completed_probes: Arc::new(AtomicUsize::new(0)),
            probe_timeout: MODEL_PROBE_TIMEOUT
                .min(Duration::from_secs(config.connect_timeout_secs.max(1))),
            background_refreshing: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn config(&self) -> &LlmConfig {
        &self.runtime_config
    }

    pub fn matches_config(&self, config: &LlmConfig) -> bool {
        self.runtime_config.as_ref() == config
    }

    pub async fn text(&self) -> std::result::Result<LlmRoleBinding, AssistantUnavailable> {
        self.binding(LlmRole::Text).await
    }

    pub async fn vision(&self) -> std::result::Result<LlmRoleBinding, AssistantUnavailable> {
        self.binding(LlmRole::Vision).await
    }

    async fn binding(
        &self,
        role: LlmRole,
    ) -> std::result::Result<LlmRoleBinding, AssistantUnavailable> {
        if !self.base_configured {
            return Err(AssistantUnavailable::NotConfigured {
                role,
                reason: "llm.base_url is empty".into(),
            });
        }
        let model = self.model(role);
        if model.is_empty() {
            let key = match role {
                LlmRole::Text => "llm.text_model",
                LlmRole::Vision => "llm.vision_model",
            };
            return Err(AssistantUnavailable::NotConfigured {
                role,
                reason: format!("{key} is empty"),
            });
        }
        let models =
            self.refresh(false)
                .await
                .map_err(|error| AssistantUnavailable::Unreachable {
                    reason: error.to_string(),
                })?;
        let probed = models
            .iter()
            .find(|candidate| candidate.id == model)
            .ok_or_else(|| AssistantUnavailable::ModelNotFound {
                model: model.to_owned(),
            })?;
        let (supports_tools, supports_vision) = self.capabilities(probed);
        match role {
            LlmRole::Vision if !supports_vision => {
                return Err(AssistantUnavailable::CapabilityUnsupported {
                    model: model.to_owned(),
                    capability: LlmCapability::Vision,
                })
            }
            _ => {}
        }
        Ok(LlmRoleBinding {
            provider: self.provider.as_ref().expect("configured provider").clone(),
            model: model.to_owned(),
            supports_tools,
            supports_vision,
        })
    }

    pub async fn refresh(&self, force: bool) -> Result<Vec<LlmModel>> {
        let provider = self.provider.as_ref().ok_or_else(|| {
            HarnessError::Configuration("LLM is not configured; set llm.base_url".into())
        })?;
        let observed_probe = self.completed_probes.load(Ordering::Acquire);
        if !force {
            if let Some(result) = self.cached_result() {
                return result;
            }
        }
        let _probe = self.probe.lock().await;
        if (!force && self.cached_result().is_some())
            || self.completed_probes.load(Ordering::Acquire) != observed_probe
        {
            return self
                .cached_result()
                .expect("a completed probe has a cache result");
        }
        let probe = tokio::time::timeout(self.probe_timeout, provider.list_models()).await;
        let result = match probe {
            Ok(Ok(models)) => {
                *self.cache.write().expect("LLM cache lock") = ProbeCache::Ready {
                    models: models.clone(),
                    at: Instant::now(),
                };
                Ok(models)
            }
            Ok(Err(error)) => {
                let reason = error.to_string();
                *self.cache.write().expect("LLM cache lock") = ProbeCache::Failed {
                    reason: reason.clone(),
                    at: Instant::now(),
                };
                Err(HarnessError::Backend {
                    backend: "llm".into(),
                    message: reason,
                })
            }
            Err(_) => {
                let reason = format!(
                    "model listing timed out after {} seconds",
                    self.probe_timeout.as_secs_f64()
                );
                *self.cache.write().expect("LLM cache lock") = ProbeCache::Failed {
                    reason: reason.clone(),
                    at: Instant::now(),
                };
                Err(HarnessError::Backend {
                    backend: "llm".into(),
                    message: reason,
                })
            }
        };
        self.completed_probes.fetch_add(1, Ordering::AcqRel);
        result
    }

    fn cached_result(&self) -> Option<Result<Vec<LlmModel>>> {
        let cache = self.cache.read().expect("LLM cache lock");
        if !cache.fresh(Instant::now()) {
            return None;
        }
        Some(match &*cache {
            ProbeCache::Ready { models, .. } => Ok(models.clone()),
            ProbeCache::Failed { reason, .. } => Err(HarnessError::Backend {
                backend: "llm".into(),
                message: reason.clone(),
            }),
            ProbeCache::Pending => unreachable!(),
        })
    }

    pub fn refresh_in_background(&self) {
        if self.provider.is_none()
            || self.background_refreshing.load(Ordering::Acquire)
            || self
                .cache
                .read()
                .expect("LLM cache lock")
                .fresh(Instant::now())
        {
            return;
        }
        if self
            .background_refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let roles = self.clone();
        tokio::spawn(async move {
            let _ = roles.refresh(false).await;
            roles.background_refreshing.store(false, Ordering::Release);
        });
    }

    pub fn health(&self) -> LlmHealthSnapshot {
        if !self.base_configured {
            return LlmHealthSnapshot {
                status: LlmRoleStatus::NotConfigured,
                reason: Some("Set llm.base_url to enable the assistant.".into()),
                text: Some(self.not_configured_role(LlmRole::Text)),
                vision: Some(self.not_configured_role(LlmRole::Vision)),
                models: Vec::new(),
            };
        }
        let cache = self.cache.read().expect("LLM cache lock").clone();
        match cache {
            ProbeCache::Pending => LlmHealthSnapshot {
                status: LlmRoleStatus::Unavailable,
                reason: Some("The model capability probe is pending.".into()),
                text: Some(self.pending_role(LlmRole::Text)),
                vision: Some(self.pending_role(LlmRole::Vision)),
                models: Vec::new(),
            },
            ProbeCache::Failed { reason, .. } => LlmHealthSnapshot {
                status: LlmRoleStatus::Unavailable,
                reason: Some(reason.clone()),
                text: Some(self.unavailable_role(LlmRole::Text, reason.clone())),
                vision: Some(self.unavailable_role(LlmRole::Vision, reason)),
                models: Vec::new(),
            },
            ProbeCache::Ready { models, .. } => {
                let text = Some(self.role_health(LlmRole::Text, &models));
                let vision = Some(self.role_health(LlmRole::Vision, &models));
                let configured_roles = [&text, &vision]
                    .iter()
                    .filter_map(|role| role.as_ref())
                    .filter(|role| role.status != LlmRoleStatus::NotConfigured)
                    .collect::<Vec<_>>();
                let status = if !configured_roles.is_empty()
                    && configured_roles
                        .iter()
                        .all(|role| role.status == LlmRoleStatus::Ok)
                {
                    LlmRoleStatus::Ok
                } else {
                    LlmRoleStatus::Unavailable
                };
                let reason = (status != LlmRoleStatus::Ok).then(|| {
                    text.as_ref()
                        .and_then(|role| role.reason.clone())
                        .or_else(|| vision.as_ref().and_then(|role| role.reason.clone()))
                        .unwrap_or_else(|| "No LLM model role is configured.".into())
                });
                LlmHealthSnapshot {
                    status,
                    reason,
                    text,
                    vision,
                    models: models
                        .into_iter()
                        .map(|model| {
                            let (supports_tools, supports_vision) = self.capabilities(&model);
                            LlmModelSnapshot {
                                id: model.id,
                                supports_tools,
                                supports_vision,
                            }
                        })
                        .collect(),
                }
            }
        }
    }

    fn model(&self, role: LlmRole) -> &str {
        match role {
            LlmRole::Text => &self.text_model,
            LlmRole::Vision => &self.vision_model,
        }
    }

    pub(crate) fn capabilities(&self, model: &LlmModel) -> (bool, bool) {
        let override_capabilities = self.capability_overrides.get(&model.id);
        (
            override_capabilities
                .and_then(|value| value.tools)
                .unwrap_or(model.supports_tools),
            override_capabilities
                .and_then(|value| value.vision)
                .unwrap_or(model.supports_vision),
        )
    }

    fn role_health(&self, role: LlmRole, models: &[LlmModel]) -> LlmRoleHealth {
        let model = self.model(role);
        if model.is_empty() {
            return self.not_configured_role(role);
        }
        let Some(probed) = models.iter().find(|candidate| candidate.id == model) else {
            return self.unavailable_role(role, format!("Model '{model}' was not found."));
        };
        let (_, vision) = self.capabilities(probed);
        let capability = match role {
            LlmRole::Text => None,
            LlmRole::Vision => (!vision).then_some("vision"),
        };
        if let Some(capability) = capability {
            return self.unavailable_role(
                role,
                format!("Model '{model}' lacks the required {capability} capability."),
            );
        }
        LlmRoleHealth {
            status: LlmRoleStatus::Ok,
            model: Some(model.to_owned()),
            reason: None,
        }
    }

    fn not_configured_role(&self, role: LlmRole) -> LlmRoleHealth {
        let model = self.model(role);
        LlmRoleHealth {
            status: LlmRoleStatus::NotConfigured,
            model: (!model.is_empty()).then(|| model.to_owned()),
            reason: Some(if model.is_empty() {
                format!("llm.{}_model is empty.", role_name(role))
            } else if !self.base_configured {
                "llm.base_url is empty.".into()
            } else {
                "The role is not configured.".into()
            }),
        }
    }

    fn pending_role(&self, role: LlmRole) -> LlmRoleHealth {
        LlmRoleHealth {
            status: if self.model(role).is_empty() {
                LlmRoleStatus::NotConfigured
            } else {
                LlmRoleStatus::Unavailable
            },
            model: (!self.model(role).is_empty()).then(|| self.model(role).to_owned()),
            reason: Some("Waiting for the model capability probe.".into()),
        }
    }

    fn unavailable_role(&self, role: LlmRole, reason: String) -> LlmRoleHealth {
        if self.model(role).is_empty() {
            return self.not_configured_role(role);
        }
        LlmRoleHealth {
            status: LlmRoleStatus::Unavailable,
            model: Some(self.model(role).to_owned()),
            reason: Some(reason),
        }
    }
}

const fn role_name(role: LlmRole) -> &'static str {
    match role {
        LlmRole::Text => "text",
        LlmRole::Vision => "vision",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{ChatStream, LlmModel};
    use crate::{async_trait, ToolDefinition};
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Default)]
    struct FakeProvider {
        calls: Arc<AtomicUsize>,
        models: Vec<LlmModel>,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn chat_stream(&self, _request: ChatRequest) -> Result<ChatStream> {
            unreachable!()
        }
        async fn list_models(&self) -> Result<Vec<LlmModel>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.models.clone())
        }
    }

    fn config() -> LlmConfig {
        LlmConfig {
            base_url: "http://127.0.0.1:11434/v1".into(),
            text_model: "text".into(),
            vision_model: "vision".into(),
            ..LlmConfig::default()
        }
    }

    #[tokio::test]
    async fn text_binds_without_tools_while_request_checks_and_overrides_stay_strict() {
        let provider = Arc::new(FakeProvider {
            models: vec![
                LlmModel {
                    id: "text".into(),
                    supports_tools: false,
                    supports_vision: false,
                },
                LlmModel {
                    id: "vision".into(),
                    supports_tools: false,
                    supports_vision: false,
                },
            ],
            ..FakeProvider::default()
        });
        let mut debug_config = config();
        debug_config.api_key = Some("roles-debug-secret".into());
        let roles = LlmRoles::new(&debug_config, Some(provider.clone()));
        assert!(!format!("{roles:?}").contains("roles-debug-secret"));
        let text = roles.text().await.unwrap();
        assert_eq!(text.model(), "text");
        let tool_free_request = ChatRequest {
            model: "text".into(),
            messages: vec![ChatMessage::User(vec![ChatContent::Text("hello".into())])],
            tools: vec![],
        };
        assert!(text.check_request(&tool_free_request).is_ok());
        let tools_request = ChatRequest {
            model: "text".into(),
            messages: vec![],
            tools: vec![ToolDefinition {
                name: "review".into(),
                description: "Review a proposal".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        };
        let error = text.check_request(&tools_request).unwrap_err();
        assert!(matches!(error, HarnessError::Configuration(_)));
        let error = error.to_string();
        assert!(error.contains("text") && error.contains("tools"));
        assert!(matches!(
            roles.vision().await,
            Err(AssistantUnavailable::CapabilityUnsupported {
                capability: LlmCapability::Vision,
                ..
            })
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1, "cache is reused");

        let mut overridden = config();
        overridden.capabilities.insert(
            "text".into(),
            LlmCapabilityOverride {
                tools: Some(true),
                vision: None,
            },
        );
        overridden.capabilities.insert(
            "vision".into(),
            LlmCapabilityOverride {
                tools: None,
                vision: Some(true),
            },
        );
        let roles = LlmRoles::new(&overridden, Some(provider));
        let text = roles.text().await.unwrap();
        assert_eq!(text.model(), "text");
        let vision = roles.vision().await.unwrap();
        assert_eq!(vision.model(), "vision");
        let request = ChatRequest {
            model: "vision".into(),
            messages: vec![ChatMessage::User(vec![ChatContent::Image {
                media_type: "image/png".into(),
                bytes: vec![1],
            }])],
            tools: vec![],
        };
        assert!(vision.check_request(&request).is_ok());
    }

    #[tokio::test]
    async fn check_request_names_mismatched_model_and_known_unsupported_capability() {
        let provider = Arc::new(FakeProvider {
            models: vec![LlmModel {
                id: "text".into(),
                supports_tools: false,
                supports_vision: false,
            }],
            ..FakeProvider::default()
        });
        let mut config = config();
        config.capabilities.insert(
            "text".into(),
            LlmCapabilityOverride {
                tools: Some(true),
                vision: Some(false),
            },
        );
        let roles = LlmRoles::new(&config, Some(provider));
        let text = roles.text().await.unwrap();
        let wrong_model = ChatRequest {
            model: "other".into(),
            messages: vec![],
            tools: vec![],
        };
        assert!(text
            .check_request(&wrong_model)
            .unwrap_err()
            .to_string()
            .contains("other"));
        let tools_and_image = ChatRequest {
            model: "text".into(),
            messages: vec![ChatMessage::User(vec![ChatContent::Image {
                media_type: "image/png".into(),
                bytes: vec![],
            }])],
            tools: vec![],
        };
        let error = text
            .check_request(&tools_and_image)
            .unwrap_err()
            .to_string();
        assert!(error.contains("text") && error.contains("vision"));
    }
}
