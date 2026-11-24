use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, HarnessError>;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("configuration error: {key}='{value}': {reason}")]
    InvalidSetting {
        key: String,
        value: String,
        reason: String,
    },
    #[error("{0}")]
    SettingsConflict(&'static str),
    #[error("{reason}")]
    SettingsDatabaseConflict {
        key: &'static str,
        reason: &'static str,
    },
    #[error("workflow validation failed: {0}")]
    Workflow(String),
    #[error("The workflow changed since you reviewed it.")]
    WorkflowChanged,
    #[error("cannot resolve workflow ID '{id}' unambiguously: workflow directory or entry '{}' could not be inspected ({reason}); fix permissions or repair the path and retry", path.display())]
    WorkflowDiscoveryIncomplete {
        id: String,
        path: PathBuf,
        reason: String,
    },
    #[error("ComfyUI request failed: {0}")]
    ComfyUi(String),
    #[error("{backend} request failed: {message}")]
    Backend { backend: String, message: String },
    #[error("state database error: {0}")]
    Database(String),
    #[error("run '{run_id}' was not found")]
    RunNotFound { run_id: String },
    #[error("assistant conversation '{conversation_id}' was not found")]
    ConversationNotFound { conversation_id: String },
    #[error("{0}")]
    InvalidConversationRequest(String),
    #[error("{0}")]
    InvalidPromptAssistRequest(String),
    #[error("prompt assist provider response failed: {0}")]
    PromptAssistProvider(String),
    #[error("{0}")]
    ConversationConflict(String),
    #[error("assistant tool confirmation '{confirmation_id}' was not found in conversation '{conversation_id}'")]
    ToolConfirmationNotFound {
        conversation_id: String,
        confirmation_id: String,
    },
    #[error("{0}")]
    ToolConfirmationConflict(String),
    #[error("{0}")]
    InvalidToolArguments(String),
    #[error("assistant not configured: {0}")]
    AssistantNotConfigured(String),
    #[error("this idempotency key already belongs to a different request")]
    IdempotencyConflict,
    #[error("{0}")]
    RerunMismatch(&'static str),
    #[error("The source run has no submission-time workflow manifest fingerprint.")]
    RerunMissingManifest,
    #[error("The workflow references a ComfyUI-side file that GenAImage cannot verify (node '{node}', input '{input}').")]
    RerunUnverifiedMedia { node: String, input: &'static str },
    #[error("the server is shutting down")]
    ShuttingDown,
    #[error("the cancellation request was not sent: {0}")]
    CancellationNotSent(String),
    #[error("the backend rejected the cancellation request: {0}")]
    CancellationRejected(String),
    #[error("the cancellation outcome is unknown: {0}")]
    CancellationUnknown(String),
    #[error("{0}")]
    UnsupportedOperation(String),
    #[error("{0}")]
    InvalidRunRequest(String),
    #[error(transparent)]
    UploadStorage(#[from] crate::uploads::UploadError),
    #[error("{0}")]
    RunConflict(String),
    #[error("Output was not found.")]
    OutputNotFound,
    #[error("vision review unavailable: {reason}")]
    VisionReviewUnavailable { reason: String },
    #[error("vision review input is invalid: {0}")]
    VisionReviewInput(String),
    #[error("vision review image is {byte_size} bytes; the limit is {limit} bytes")]
    VisionReviewImageTooLarge { byte_size: u64, limit: u64 },
    #[error("vision review unsupported image: {0}")]
    VisionReviewUnsupportedImage(String),
    #[error("vision model returned invalid JSON: {0}")]
    VisionReviewResponse(String),
    #[error("No completed manifest is recorded for this run.")]
    ManifestNotFound,
    #[error("{0}")]
    InvalidMediaRequest(&'static str),
    #[error("The stored media does not match its provenance.")]
    MediaIntegrity,
    #[error(
        "The output could not be decoded as a supported image within the thumbnail limits: {0}"
    )]
    ThumbnailDecode(#[source] image::ImageError),
    #[error("path '{path}' is not valid: {reason}")]
    InvalidPath { path: PathBuf, reason: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
