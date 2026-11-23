use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, HarnessError>;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("workflow validation failed: {0}")]
    Workflow(String),
    #[error("ComfyUI request failed: {0}")]
    ComfyUi(String),
    #[error("{backend} request failed: {message}")]
    Backend { backend: String, message: String },
    #[error("state database error: {0}")]
    Database(String),
    #[error("run '{run_id}' was not found")]
    RunNotFound { run_id: String },
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
