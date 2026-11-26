use std::pin::Pin;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{header, Method, StatusCode};

use super::{events::with_safe_preamble, has_content_type, Client, Error};
use crate::{
    CheckpointsResponse, LlmModelsResponse, ModelPull, ModelPullListResponse, StartModelPullRequest,
};

/// The outcome of asking for a pull.
#[derive(Clone, Debug)]
pub enum PullStart {
    Started(ModelPull),
    /// The model is already being pulled; follow this pull ID.
    AlreadyRunning {
        id: String,
    },
}

/// Snapshots of one pull. The stream ends after the terminal snapshot
/// (`succeeded`, `failed` or `cancelled`); ending earlier means the connection
/// was lost, so read the pull again instead of assuming an outcome.
pub type ModelPullStream = Pin<Box<dyn Stream<Item = Result<ModelPull, Error>> + Send>>;

impl Client {
    pub async fn llm_models(&self) -> Result<LlmModelsResponse, Error> {
        self.json(self.get(&["models", "llm"])?).await
    }

    /// Starts a download. Call only for an explicit user action. A 409 means
    /// the model is already being pulled: attach to that pull, never post again.
    pub async fn start_model_pull(&self, name: &str) -> Result<PullStart, Error> {
        let request = self
            .request(Method::POST, &["models", "llm", "pulls"])?
            .json(&StartModelPullRequest {
                name: name.to_owned(),
            })
            .build()?;
        let response = self.execute(request).await?;
        if response.status() == StatusCode::CONFLICT {
            let existing = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| location.rsplit('/').next())
                .filter(|id| !id.is_empty());
            if let Some(id) = existing {
                return Ok(PullStart::AlreadyRunning { id: id.to_owned() });
            }
        }
        Ok(PullStart::Started(
            Self::check_status(response, false, false)
                .await?
                .json()
                .await?,
        ))
    }

    pub async fn model_pulls(&self) -> Result<ModelPullListResponse, Error> {
        self.json(self.get(&["models", "llm", "pulls"])?).await
    }

    pub async fn model_pull(&self, id: &str) -> Result<ModelPull, Error> {
        self.json(self.get(&["models", "llm", "pulls", id])?).await
    }

    pub async fn cancel_model_pull(&self, id: &str) -> Result<ModelPull, Error> {
        self.json(self.request(Method::DELETE, &["models", "llm", "pulls", id])?)
            .await
    }

    /// Attaches to a pull: the first item is its current snapshot.
    pub async fn model_pull_events(&self, id: &str) -> Result<ModelPullStream, Error> {
        let request = self
            .get(&["models", "llm", "pulls", id, "events"])?
            .header(header::ACCEPT, "text/event-stream");
        let response = self.send(request, false).await?;
        if !has_content_type(&response, "text/event-stream") {
            return Err(Error::UnexpectedEventContentType);
        }
        Ok(Box::pin(
            with_safe_preamble(response.bytes_stream())
                .eventsource()
                .map(|frame| Ok(serde_json::from_str(&frame?.data)?)),
        ))
    }

    pub async fn checkpoints(&self) -> Result<CheckpointsResponse, Error> {
        self.json(self.get(&["models", "checkpoints"])?).await
    }
}
