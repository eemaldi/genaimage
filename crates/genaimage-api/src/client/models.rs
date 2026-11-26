use std::pin::Pin;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{header, Method};

use super::{events::with_safe_preamble, has_content_type, Client, Error};
use crate::{
    CheckpointsResponse, LlmModelsResponse, ModelPull, ModelPullListResponse, StartModelPullRequest,
};

/// Snapshots of one pull. The stream ends after the terminal snapshot
/// (`succeeded`, `failed` or `cancelled`); ending earlier means the connection
/// was lost, so read the pull again instead of assuming an outcome.
pub type ModelPullStream = Pin<Box<dyn Stream<Item = Result<ModelPull, Error>> + Send>>;

impl Client {
    pub async fn llm_models(&self) -> Result<LlmModelsResponse, Error> {
        self.json(self.get(&["models", "llm"])?).await
    }

    /// Starts a download. Call only for an explicit user action.
    pub async fn start_model_pull(&self, name: &str) -> Result<ModelPull, Error> {
        self.json(
            self.request(Method::POST, &["models", "llm", "pulls"])?
                .json(&StartModelPullRequest {
                    name: name.to_owned(),
                }),
        )
        .await
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
