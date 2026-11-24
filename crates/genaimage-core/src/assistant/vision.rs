use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use image::ImageFormat;
use serde::{Deserialize, Serialize};

use crate::{
    artifacts::RunManifest,
    extensions::{ChatContent, ChatDelta, ChatMessage, ChatRequest, LlmProvider},
    media::blocking,
    Harness, HarnessError, Result,
};

const MAX_VISION_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_REVIEW_PROMPT_BYTES: u64 = 64 * 1024;
const MAX_MODEL_RESPONSE_BYTES: usize = 1024 * 1024;
const DEFAULT_REVIEW_PROMPT: &str = r#"Review the supplied generated image against the recorded generation prompt and parameters below.

Return only one strict JSON object with exactly these fields:
- "caption": a concise, factual caption of the image
- "adherence_critique": a concise critique of adherence to the recorded request, identifying material omissions or contradictions
- "tags": an array of short, lowercase string tags relevant to the image

The response is a model opinion, not a factual provenance record. Do not use Markdown or add fields.

Recorded generation prompt and parameters:
<run_request>
{{run_prompt}}
</run_request>
"#;

/// A single append-only model-opinion annotation for an indexed output.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OutputAnnotation {
    pub id: i64,
    pub output_id: String,
    pub run_id: String,
    pub kind: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub caption: String,
    pub adherence_critique: String,
    pub tags: Vec<String>,
    /// The complete prompt sent to the model, including the recorded run request.
    pub prompt: String,
    /// The exact concatenated text received from the model.
    pub raw_response: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VisionResponse {
    caption: String,
    adherence_critique: String,
    tags: Vec<String>,
}

/// Core service for creating and listing model-opinion vision annotations.
///
/// The image is sent byte-for-byte. Oversized and unsupported images are
/// rejected rather than silently transcoded or downscaled.
#[derive(Clone)]
pub struct VisionReviewService {
    harness: Harness,
    provider: Arc<dyn LlmProvider>,
    vision_model: Option<String>,
}

impl VisionReviewService {
    /// `vision_model` must be the explicitly configured vision model. Pass
    /// `None` when configuration has no vision model; no text-model fallback
    /// is performed.
    pub fn new(
        harness: Harness,
        provider: Arc<dyn LlmProvider>,
        vision_model: Option<String>,
    ) -> Self {
        Self {
            harness,
            provider,
            vision_model,
        }
    }

    /// Review an indexed output using one vision chat request and append one
    /// annotation. A failed or malformed model response stores nothing.
    pub async fn review_output(&self, output_id: &str) -> Result<OutputAnnotation> {
        let model = self
            .vision_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| HarnessError::VisionReviewUnavailable {
                reason: "no vision model is configured".to_owned(),
            })?
            .to_owned();

        let repository = self.harness.repository().as_async();
        let indexed = repository.output(output_id).await?;
        if indexed.output.byte_size > MAX_VISION_IMAGE_BYTES {
            return Err(HarnessError::VisionReviewImageTooLarge {
                byte_size: indexed.output.byte_size,
                limit: MAX_VISION_IMAGE_BYTES,
            });
        }
        let run_id = indexed.output.run_id.clone();
        let (output, media) = self.harness.open_output(output_id).await?;
        if output.output.run_id != run_id {
            return Err(HarnessError::MediaIntegrity);
        }
        let manifest = self.harness.open_run_manifest(&run_id).await?;
        let data_dir = self.harness.config().data_dir.clone();
        let manifest_run_id = run_id.clone();
        let (bytes, prompt, media_type) = blocking(move || {
            let mut bytes = Vec::new();
            media
                .file
                .take(MAX_VISION_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_VISION_IMAGE_BYTES {
                return Err(HarnessError::VisionReviewImageTooLarge {
                    byte_size: bytes.len() as u64,
                    limit: MAX_VISION_IMAGE_BYTES,
                });
            }
            let media_type = supported_media_type(&bytes)?;
            let mut manifest_file = manifest.media.file;
            let manifest: RunManifest = serde_json::from_reader(&mut manifest_file)
                .map_err(|_| HarnessError::MediaIntegrity)?;
            if manifest.run_id != manifest_run_id {
                return Err(HarnessError::MediaIntegrity);
            }
            let recorded_request = serde_json::to_string_pretty(&manifest.parameters)?;
            let prompt = review_prompt(&data_dir, &recorded_request)?;
            Ok((bytes, prompt, media_type))
        })
        .await?;

        let request = ChatRequest {
            model: model.clone(),
            messages: vec![ChatMessage::User(vec![
                ChatContent::Text(prompt.clone()),
                ChatContent::Image {
                    media_type: media_type.to_owned(),
                    bytes,
                },
            ])],
            tools: Vec::new(),
        };
        let mut stream = self.provider.chat_stream(request).await?;
        let mut raw_response = String::new();
        while let Some(delta) = stream.next().await {
            match delta? {
                ChatDelta::Text(text) => {
                    raw_response.push_str(&text);
                    if raw_response.len() > MAX_MODEL_RESPONSE_BYTES {
                        return Err(HarnessError::VisionReviewResponse(format!(
                            "response exceeds the {} byte limit",
                            MAX_MODEL_RESPONSE_BYTES
                        )));
                    }
                }
                ChatDelta::ToolCall(_) => {
                    return Err(HarnessError::VisionReviewResponse(
                        "the model returned a tool call instead of review JSON".to_owned(),
                    ));
                }
            }
        }
        let parsed: VisionResponse = serde_json::from_str(&raw_response)
            .map_err(|error| HarnessError::VisionReviewResponse(error.to_string()))?;
        let annotation = OutputAnnotation {
            id: 0,
            output_id: output_id.to_owned(),
            run_id,
            kind: "vision_review".to_owned(),
            model,
            created_at: Utc::now(),
            caption: parsed.caption,
            adherence_critique: parsed.adherence_critique,
            tags: parsed.tags,
            prompt,
            raw_response,
        };
        repository.insert_output_annotation(annotation).await
    }

    /// List at most the newest 100 append-only annotations for an output.
    pub async fn annotations_for_output(&self, output_id: &str) -> Result<Vec<OutputAnnotation>> {
        let repository = self.harness.repository().as_async();
        repository.output(output_id).await?;
        repository.output_annotations(output_id).await
    }
}

fn supported_media_type(bytes: &[u8]) -> Result<&'static str> {
    let format = image::guess_format(bytes).map_err(|_| {
        HarnessError::VisionReviewUnsupportedImage("image type could not be determined".to_owned())
    })?;
    match format {
        ImageFormat::Png => Ok("image/png"),
        ImageFormat::Jpeg => Ok("image/jpeg"),
        ImageFormat::WebP => Ok("image/webp"),
        _ => Err(HarnessError::VisionReviewUnsupportedImage(format!(
            "type {format:?}; use PNG, JPEG or WebP"
        ))),
    }
}

fn review_prompt(data_dir: &Path, recorded_request: &str) -> Result<String> {
    let directory = data_dir.join("assistant/prompts");
    fs::create_dir_all(&directory)?;
    let path = directory.join("vision_review.md");
    let template = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(DEFAULT_REVIEW_PROMPT.as_bytes())?;
            DEFAULT_REVIEW_PROMPT.to_owned()
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new().read(true).open(&path)?;
            if file.metadata()?.len() > MAX_REVIEW_PROMPT_BYTES {
                return Err(HarnessError::VisionReviewInput(format!(
                    "prompt template '{}' exceeds the {} byte limit",
                    path.display(),
                    MAX_REVIEW_PROMPT_BYTES
                )));
            }
            let mut template = String::new();
            file.take(MAX_REVIEW_PROMPT_BYTES + 1)
                .read_to_string(&mut template)?;
            if template.len() as u64 > MAX_REVIEW_PROMPT_BYTES {
                return Err(HarnessError::VisionReviewInput(format!(
                    "prompt template '{}' exceeds the {} byte limit",
                    path.display(),
                    MAX_REVIEW_PROMPT_BYTES
                )));
            }
            template
        }
        Err(error) => return Err(error.into()),
    };
    if template.contains("{{run_prompt}}") {
        Ok(template.replace("{{run_prompt}}", recorded_request))
    } else {
        Ok(format!(
            "{template}\n\nRecorded generation prompt and parameters:\n<run_request>\n{recorded_request}\n</run_request>\n"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use futures_util::stream;
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        artifacts::{InputFileRecord, RunManifest, WorkflowManifestRecord},
        async_trait,
        extensions::{ChatStream, LlmModel},
        image_backend::{
            BackendHealth, EventMonitor, HistorySnapshot, OutputReference, PromptResponse,
            QueueSnapshot, UploadResponse,
        },
        persistence::{NewRun, OutputRecord},
        workflow::GenerationMode,
        HarnessConfig, ImageBackend, RunStatus,
    };

    const REVIEW_RESPONSE: &str = r#"{"caption":"A lighthouse above the sea","adherence_critique":"The image follows the coastal setting and dawn light.","tags":["coast","dawn","lighthouse"]}"#;
    const MALFORMED_RESPONSE: &str =
        r#"{"caption":"A lighthouse","adherence_critique":"Fine","tags":[],"unexpected":true}"#;

    #[derive(Clone)]
    struct FakeLlm {
        response: Arc<String>,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    impl FakeLlm {
        fn new(response: &str) -> Self {
            Self {
                response: Arc::new(response.to_owned()),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
            self.requests.lock().unwrap().push(request);
            let response = (*self.response).clone();
            Ok(Box::pin(stream::iter([Ok(ChatDelta::Text(response))])))
        }

        async fn list_models(&self) -> Result<Vec<LlmModel>> {
            Ok(vec![LlmModel {
                id: "vision-test".to_owned(),
                supports_tools: false,
                supports_vision: true,
            }])
        }
    }

    struct FakeBackend;

    #[async_trait]
    impl ImageBackend for FakeBackend {
        fn name(&self) -> &str {
            "fake"
        }

        fn base_url(&self) -> &str {
            "fake://local"
        }

        async fn health(&self) -> Result<BackendHealth> {
            unsupported()
        }

        async fn object_info(&self) -> Result<Value> {
            unsupported()
        }

        async fn queue(&self) -> Result<QueueSnapshot> {
            unsupported()
        }

        async fn submit(&self, _: &Value, _: &str, _: &str) -> Result<PromptResponse> {
            unsupported()
        }

        async fn history(&self, _: &str) -> Result<HistorySnapshot> {
            unsupported()
        }

        async fn delete_queued(&self, _: &str) -> Result<Value> {
            unsupported()
        }

        async fn interrupt(&self, _: &str) -> Result<Value> {
            unsupported()
        }

        async fn upload_image(&self, _: &Path) -> Result<UploadResponse> {
            unsupported()
        }

        async fn download_output(&self, _: &OutputReference) -> Result<Vec<u8>> {
            unsupported()
        }

        async fn monitor_events(&self, _: &str) -> Result<EventMonitor> {
            unsupported()
        }
    }

    fn unsupported<T>() -> Result<T> {
        Err(HarnessError::UnsupportedOperation(
            "not used by vision-review tests".to_owned(),
        ))
    }

    struct Fixture {
        _directory: TempDir,
        harness: Harness,
        output_id: String,
        image: Vec<u8>,
        prompt_path: PathBuf,
        provenance_paths: Vec<PathBuf>,
    }

    async fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let config =
            HarnessConfig::load_with(directory.path(), None, directory.path(), |key| match key {
                "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => {
                    Some(directory.path().display().to_string())
                }
                _ => None,
            })
            .unwrap();
        let prompt_path = config.data_dir.join("assistant/prompts/vision_review.md");
        let harness = Harness::new(config, Arc::new(FakeBackend)).unwrap();
        let run_id = "review-run";
        let output_dir = harness.config().runs.join(run_id);
        fs::create_dir_all(output_dir.join("outputs")).unwrap();
        let workflow_path = output_dir.join("workflow-manifest.json");
        let resolved_path = output_dir.join("resolved-workflow.json");
        let preflight_path = output_dir.join("preflight.json");
        let manifest_path = output_dir.join("run-manifest.json");
        fs::write(&workflow_path, br#"{"id":"test","tags":[]}"#).unwrap();
        fs::write(&resolved_path, br#"{"1":{"class_type":"SaveImage"}}"#).unwrap();
        fs::write(
            &preflight_path,
            br#"{"schema_version":1,"parameters":{"prompt":"A lighthouse at dawn"}}"#,
        )
        .unwrap();

        let parameters = json!({"prompt": "A lighthouse at dawn", "seed": 42});
        let run = harness
            .repository()
            .insert(&NewRun {
                id: run_id.to_owned(),
                prompt_id: "prompt-review-run".to_owned(),
                client_id: "client-review-run".to_owned(),
                workflow_id: "review-workflow".to_owned(),
                workflow_version: "1".to_owned(),
                workflow_sha256: "a".repeat(64),
                workflow_manifest_sha256: Some("b".repeat(64)),
                rerun_of: None,
                generation_mode: GenerationMode::Generic,
                status: RunStatus::Completed,
                parameters: parameters.clone(),
                output_dir: output_dir.display().to_string(),
            })
            .unwrap();
        harness
            .repository()
            .update_status(
                run_id,
                RunStatus::Completed,
                None,
                Some(manifest_path.display().to_string().as_str()),
            )
            .unwrap();

        let mut cursor = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(3, 2)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        let image = cursor.into_inner();
        let output_path = output_dir.join("outputs/result.png");
        fs::write(&output_path, &image).unwrap();
        let now = Utc::now();
        let output = OutputRecord {
            run_id: run_id.to_owned(),
            node_id: "9".to_owned(),
            kind: "image".to_owned(),
            source_filename: "result.png".to_owned(),
            source_subfolder: String::new(),
            source_type: "output".to_owned(),
            local_path: output_path.display().to_string(),
            sha256: hex::encode(Sha256::digest(&image)),
            byte_size: image.len() as u64,
            created_at: now,
        };
        let repository = harness.repository().as_async();
        repository.record_output(&output).await.unwrap();
        let manifest = RunManifest {
            schema_version: 1,
            run_id: run_id.to_owned(),
            rerun_of: None,
            prompt_id: run.prompt_id,
            client_id: run.client_id,
            workflow: WorkflowManifestRecord {
                id: run.workflow_id,
                version: run.workflow_version,
                sha256: run.workflow_sha256,
                manifest_sha256: "b".repeat(64),
                manifest_path: workflow_path.display().to_string(),
            },
            resolved_workflow_sha256: Some("c".repeat(64)),
            preset: None,
            parameters,
            comfyui_base_url: "fake://local".to_owned(),
            source_inputs: Vec::<InputFileRecord>::new(),
            outputs: vec![output],
            created_at: run.created_at,
            completed_at: now,
            comfyui_history: json!({}),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        repository
            .record_run_media(
                run_id.to_owned(),
                hex::encode(Sha256::digest(&manifest_bytes)),
                Vec::new(),
            )
            .await
            .unwrap();

        Fixture {
            _directory: directory,
            harness,
            output_id: format!("{run_id}:1"),
            image,
            prompt_path,
            provenance_paths: vec![workflow_path, resolved_path, preflight_path, manifest_path],
        }
    }

    #[tokio::test]
    async fn successful_review_stores_opinion_and_sends_exact_image_and_prompt() {
        let fixture = fixture().await;
        fs::create_dir_all(fixture.prompt_path.parent().unwrap()).unwrap();
        let custom_template = "Operator template: review {{run_prompt}}";
        fs::write(&fixture.prompt_path, custom_template).unwrap();
        let run_before =
            serde_json::to_vec(&fixture.harness.get_run("review-run").unwrap()).unwrap();
        let provenance_before: Vec<_> = fixture
            .provenance_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let provider = FakeLlm::new(REVIEW_RESPONSE);
        let service = VisionReviewService::new(
            fixture.harness.clone(),
            Arc::new(provider.clone()),
            Some("vision-test".to_owned()),
        );

        let annotation = service.review_output(&fixture.output_id).await.unwrap();

        assert!(annotation.id > 0);
        assert_eq!(annotation.kind, "vision_review");
        assert_eq!(annotation.model, "vision-test");
        assert_eq!(annotation.caption, "A lighthouse above the sea");
        assert_eq!(
            annotation.adherence_critique,
            "The image follows the coastal setting and dawn light."
        );
        assert_eq!(annotation.tags, ["coast", "dawn", "lighthouse"]);
        assert_eq!(annotation.raw_response, REVIEW_RESPONSE);
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].model, "vision-test");
            let ChatMessage::User(contents) = &requests[0].messages[0] else {
                panic!("review request must be a user message")
            };
            assert!(
                matches!(&contents[0], ChatContent::Text(text) if text.contains("Operator template") && text.contains("A lighthouse at dawn"))
            );
            assert!(
                matches!(&contents[1], ChatContent::Image { media_type, bytes } if media_type == "image/png" && bytes == &fixture.image)
            );
        }
        assert_eq!(
            fs::read_to_string(&fixture.prompt_path).unwrap(),
            custom_template
        );
        assert_eq!(
            service
                .annotations_for_output(&fixture.output_id)
                .await
                .unwrap(),
            vec![annotation]
        );
        assert_eq!(
            serde_json::to_vec(&fixture.harness.get_run("review-run").unwrap()).unwrap(),
            run_before
        );
        for (path, before) in fixture.provenance_paths.iter().zip(provenance_before) {
            assert_eq!(fs::read(path).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn malformed_json_is_rejected_without_storing_an_annotation() {
        let fixture = fixture().await;
        let service = VisionReviewService::new(
            fixture.harness.clone(),
            Arc::new(FakeLlm::new(MALFORMED_RESPONSE)),
            Some("vision-test".to_owned()),
        );

        let error = service.review_output(&fixture.output_id).await.unwrap_err();

        assert!(matches!(error, HarnessError::VisionReviewResponse(_)));
        assert!(service
            .annotations_for_output(&fixture.output_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn missing_vision_model_is_unavailable_without_a_provider_request() {
        let fixture = fixture().await;
        let provider = FakeLlm::new(REVIEW_RESPONSE);
        let service =
            VisionReviewService::new(fixture.harness.clone(), Arc::new(provider.clone()), None);

        let error = service.review_output(&fixture.output_id).await.unwrap_err();

        assert!(matches!(
            error,
            HarnessError::VisionReviewUnavailable { ref reason }
                if reason == "no vision model is configured"
        ));
        assert!(provider.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reviews_are_append_only_and_listed_newest_first() {
        let fixture = fixture().await;
        let service = VisionReviewService::new(
            fixture.harness.clone(),
            Arc::new(FakeLlm::new(REVIEW_RESPONSE)),
            Some("vision-test".to_owned()),
        );
        let first = service.review_output(&fixture.output_id).await.unwrap();
        let second = service.review_output(&fixture.output_id).await.unwrap();

        let annotations = service
            .annotations_for_output(&fixture.output_id)
            .await
            .unwrap();

        assert_eq!(annotations.len(), 2);
        assert_eq!(annotations[0].id, second.id);
        assert_eq!(annotations[1].id, first.id);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn unsupported_image_types_are_rejected_without_transcoding() {
        let error = supported_media_type(b"GIF89a unsupported").unwrap_err();

        assert!(matches!(
            error,
            HarnessError::VisionReviewUnsupportedImage(_)
        ));
    }

    #[test]
    fn missing_prompt_template_is_created_and_user_edit_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let first = review_prompt(directory.path(), r#"{"prompt":"first"}"#).unwrap();
        assert!(first.contains("first"));
        let path = directory.path().join("assistant/prompts/vision_review.md");
        let default = fs::read_to_string(&path).unwrap();
        fs::write(&path, "custom: {{run_prompt}}").unwrap();

        let second = review_prompt(directory.path(), "edited").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "custom: {{run_prompt}}");
        assert_eq!(second, "custom: edited");
        assert!(default.contains("strict JSON object"));
    }
}
