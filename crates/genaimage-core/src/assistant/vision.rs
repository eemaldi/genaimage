use std::{io::Read, path::Path};

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use image::ImageFormat;
use serde::{Deserialize, Serialize};

use crate::{
    artifacts::RunManifest,
    assistant::{
        spec_review::{spec_review_prompt, validate_spec_review, SPEC_REVIEW_KIND},
        PromptTemplates, SpecReview,
    },
    characters::CharacterService,
    extensions::{ChatContent, ChatDelta, ChatMessage, ChatRequest},
    llm_roles::LlmRoleBinding,
    media::blocking,
    Harness, HarnessError, Result,
};

const MAX_VISION_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_MODEL_RESPONSE_BYTES: usize = 1024 * 1024;
pub const VISION_REVIEW_PROMPT_TEMPLATE: &str = "vision_review";
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
    /// The validated checklist of a `spec_review`; `None` for a `vision_review`.
    #[serde(default)]
    pub spec_review: Option<SpecReview>,
}

struct ReviewImage {
    run_id: String,
    bytes: Vec<u8>,
    media_type: &'static str,
    recorded_request: String,
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
}

impl VisionReviewService {
    pub fn new(harness: Harness) -> Self {
        Self { harness }
    }

    /// Review an indexed output with the exact vision role binding selected by
    /// the caller. A failed or malformed model response stores nothing.
    pub async fn review_output(
        &self,
        binding: &LlmRoleBinding,
        output_id: &str,
    ) -> Result<OutputAnnotation> {
        let model = binding.model().to_owned();
        let image = self.load_image(output_id).await?;
        let prompt =
            review_prompt(&self.harness.config().data_dir, &image.recorded_request).await?;
        let raw_response = self.ask(binding, &prompt, &image).await?;
        let parsed: VisionResponse = serde_json::from_str(&raw_response)
            .map_err(|error| HarnessError::VisionReviewResponse(error.to_string()))?;
        let annotation = OutputAnnotation {
            id: 0,
            output_id: output_id.to_owned(),
            run_id: image.run_id,
            kind: "vision_review".to_owned(),
            model,
            created_at: Utc::now(),
            caption: parsed.caption,
            adherence_critique: parsed.adherence_critique,
            tags: parsed.tags,
            prompt,
            raw_response,
            spec_review: None,
        };
        self.harness
            .repository()
            .as_async()
            .insert_output_annotation(annotation)
            .await
    }

    /// Review an indexed output against one exact character spec version with
    /// the vision role binding selected by the caller. The spec is read once and
    /// the response is validated against that snapshot. A failed or malformed
    /// response stores nothing; the result is model opinion, never a decision.
    pub async fn review_against_spec(
        &self,
        binding: &LlmRoleBinding,
        output_id: &str,
        character_id: &str,
        spec_version: u32,
    ) -> Result<OutputAnnotation> {
        let model = binding.model().to_owned();
        self.harness
            .repository()
            .as_async()
            .output(output_id)
            .await?;
        let version = CharacterService::new(self.harness.clone())
            .get_spec_version(character_id, spec_version)
            .await?;
        let image = self.load_image(output_id).await?;
        let prompt = spec_review_prompt(&self.harness.config().data_dir, &version.spec).await?;
        let raw_response = self.ask(binding, &prompt, &image).await?;
        let review = validate_spec_review(&raw_response, &version)?;
        let annotation = OutputAnnotation {
            id: 0,
            output_id: output_id.to_owned(),
            run_id: image.run_id,
            kind: SPEC_REVIEW_KIND.to_owned(),
            model,
            created_at: Utc::now(),
            caption: review.summary.clone(),
            adherence_critique: String::new(),
            tags: Vec::new(),
            prompt,
            raw_response,
            spec_review: Some(review),
        };
        self.harness
            .repository()
            .as_async()
            .insert_output_annotation(annotation)
            .await
    }

    async fn load_image(&self, output_id: &str) -> Result<ReviewImage> {
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
        let manifest_run_id = run_id.clone();
        let (bytes, recorded_request, media_type) = blocking(move || {
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
            Ok((bytes, recorded_request, media_type))
        })
        .await?;
        Ok(ReviewImage {
            run_id,
            bytes,
            media_type,
            recorded_request,
        })
    }

    async fn ask(
        &self,
        binding: &LlmRoleBinding,
        prompt: &str,
        image: &ReviewImage,
    ) -> Result<String> {
        let request = ChatRequest {
            model: binding.model().to_owned(),
            messages: vec![ChatMessage::User(vec![
                ChatContent::Text(prompt.to_owned()),
                ChatContent::Image {
                    media_type: image.media_type.to_owned(),
                    bytes: image.bytes.clone(),
                },
            ])],
            tools: Vec::new(),
        };
        binding.check_request(&request)?;
        let mut stream = binding.provider().chat_stream(request).await?;
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
                ChatDelta::ToolCall(_)
                | ChatDelta::ToolResult { .. }
                | ChatDelta::ConfirmationRequired { .. } => {
                    return Err(HarnessError::VisionReviewResponse(
                        "the model returned a tool call instead of review JSON".to_owned(),
                    ));
                }
            }
        }
        Ok(raw_response)
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

async fn review_prompt(data_dir: &Path, recorded_request: &str) -> Result<String> {
    let mut templates = PromptTemplates::new(data_dir);
    templates.register_builtin(VISION_REVIEW_PROMPT_TEMPLATE, DEFAULT_REVIEW_PROMPT)?;
    let template = templates.load(VISION_REVIEW_PROMPT_TEMPLATE).await?.content;
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
        fs,
        io::Cursor,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use futures_util::stream;
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        artifacts::{InputFileRecord, RunManifest, WorkflowManifestRecord},
        async_trait,
        config::LlmConfig,
        extensions::{ChatStream, LlmModel, LlmProvider},
        image_backend::{
            BackendHealth, EventMonitor, HistorySnapshot, OutputReference, PromptResponse,
            QueueSnapshot, UploadResponse,
        },
        persistence::{NewRun, OutputRecord},
        workflow::GenerationMode,
        HarnessConfig, ImageBackend, LlmRoles, RunStatus,
    };

    const REVIEW_RESPONSE: &str = r#"{"caption":"A lighthouse above the sea","adherence_critique":"The image follows the coastal setting and dawn light.","tags":["coast","dawn","lighthouse"]}"#;
    const MALFORMED_RESPONSE: &str =
        r#"{"caption":"A lighthouse","adherence_critique":"Fine","tags":[],"unexpected":true}"#;

    async fn vision_binding(provider: Arc<dyn LlmProvider>) -> LlmRoleBinding {
        let config = LlmConfig {
            base_url: "http://127.0.0.1:3001".into(),
            vision_model: "vision-test".into(),
            ..LlmConfig::default()
        };
        LlmRoles::new(&config, Some(provider))
            .vision()
            .await
            .unwrap()
    }

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
            checkpoint_profile: None,
            checkpoint_profile_revision: None,
            character: None,
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
        let binding = vision_binding(Arc::new(provider.clone())).await;
        let service = VisionReviewService::new(fixture.harness.clone());

        let annotation = service
            .review_output(&binding, &fixture.output_id)
            .await
            .unwrap();

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
        let binding = vision_binding(Arc::new(FakeLlm::new(MALFORMED_RESPONSE))).await;
        let service = VisionReviewService::new(fixture.harness.clone());

        let error = service
            .review_output(&binding, &fixture.output_id)
            .await
            .unwrap_err();

        assert!(matches!(error, HarnessError::VisionReviewResponse(_)));
        assert!(service
            .annotations_for_output(&fixture.output_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn missing_vision_model_is_unavailable_without_a_provider_request() {
        let provider = FakeLlm::new(REVIEW_RESPONSE);
        let config = LlmConfig {
            base_url: "http://127.0.0.1:3001".into(),
            ..LlmConfig::default()
        };
        let roles = LlmRoles::new(&config, Some(Arc::new(provider.clone())));

        let error = roles.vision().await.unwrap_err();

        assert!(matches!(
            error,
            crate::AssistantUnavailable::NotConfigured { .. }
        ));
        assert!(provider.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reviews_are_append_only_and_listed_newest_first() {
        let fixture = fixture().await;
        let binding = vision_binding(Arc::new(FakeLlm::new(REVIEW_RESPONSE))).await;
        let service = VisionReviewService::new(fixture.harness.clone());
        let first = service
            .review_output(&binding, &fixture.output_id)
            .await
            .unwrap();
        let second = service
            .review_output(&binding, &fixture.output_id)
            .await
            .unwrap();

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

    #[tokio::test]
    async fn missing_prompt_template_is_created_and_user_edit_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let first = review_prompt(directory.path(), r#"{"prompt":"first"}"#)
            .await
            .unwrap();
        assert!(first.contains("first"));
        let path = directory.path().join("assistant/prompts/vision_review.md");
        let default = fs::read_to_string(&path).unwrap();
        fs::write(&path, "custom: {{run_prompt}}").unwrap();

        let second = review_prompt(directory.path(), "edited").await.unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "custom: {{run_prompt}}");
        assert_eq!(second, "custom: edited");
        assert!(default.contains("strict JSON object"));
    }

    use crate::characters::{
        CharacterService, CharacterSpec, ElementCategory, NewCharacter, PaletteEntry, Requirement,
        SpecElement,
    };
    use crate::SpecVerdict;

    fn element(id: &str, requirement: Requirement) -> SpecElement {
        SpecElement {
            id: id.to_owned(),
            category: ElementCategory::Clothing,
            description: format!("{id} description"),
            requirement,
        }
    }

    async fn character(fixture: &Fixture) -> String {
        let created = CharacterService::new(fixture.harness.clone())
            .create_character(
                NewCharacter {
                    name: "Keeper".into(),
                    spec: Some(CharacterSpec {
                        summary: "A lighthouse keeper".into(),
                        elements: vec![
                            element("hat", Requirement::Mandatory),
                            element("cloak", Requirement::Mandatory),
                            element("lantern", Requirement::Flexible),
                            element("mood", Requirement::Unspecified),
                        ],
                        palette: vec![PaletteEntry {
                            name: "Sea blue".into(),
                            hex: Some("#1f4e79".into()),
                            role: None,
                        }],
                        views: vec![],
                        style_notes: None,
                    }),
                    note: None,
                },
                None,
            )
            .await
            .unwrap();
        created.character.id
    }

    async fn spec_review(
        fixture: &Fixture,
        character_id: &str,
        response: &str,
    ) -> (Result<OutputAnnotation>, FakeLlm) {
        let provider = FakeLlm::new(response);
        let binding = vision_binding(Arc::new(provider.clone())).await;
        let result = VisionReviewService::new(fixture.harness.clone())
            .review_against_spec(&binding, &fixture.output_id, character_id, 1)
            .await;
        (result, provider)
    }

    async fn stored(fixture: &Fixture) -> Vec<OutputAnnotation> {
        VisionReviewService::new(fixture.harness.clone())
            .annotations_for_output(&fixture.output_id)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn valid_spec_review_is_stored_with_the_exact_checklist() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let response = r#"{"elements":[{"id":"lantern","verdict":"deviation","note":"It is a torch"},{"id":"cloak","verdict":"unclear","note":"Cropped"},{"id":"hat","verdict":"met","note":"Blue cap"}],"summary":"Mostly matches"}"#;

        let (result, provider) = spec_review(&fixture, &id, response).await;
        let annotation = result.unwrap();

        let review = annotation.spec_review.clone().unwrap();
        assert_eq!(annotation.kind, "spec_review");
        assert_eq!(annotation.model, "vision-test");
        assert_eq!(annotation.caption, "Mostly matches");
        assert_eq!(annotation.raw_response, response);
        assert_eq!(review.character_id, id);
        assert_eq!(review.spec_version, 1);
        assert_eq!(review.dropped, Vec::<String>::new());
        assert_eq!(review.summary, "Mostly matches");
        let rows: Vec<_> = review
            .checklist
            .iter()
            .map(|item| (item.id.as_str(), item.verdict, item.note.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("hat", SpecVerdict::Met, "Blue cap"),
                ("cloak", SpecVerdict::Unclear, "Cropped"),
                ("lantern", SpecVerdict::Deviation, "It is a torch"),
            ]
        );
        assert!(review.label.starts_with("Model opinion, not approval"));
        assert_eq!(stored(&fixture).await, vec![annotation]);
        let requests = provider.requests.lock().unwrap();
        let ChatMessage::User(contents) = &requests[0].messages[0] else {
            panic!("expected a user message")
        };
        let ChatContent::Text(prompt) = &contents[0] else {
            panic!("expected the prompt first")
        };
        let block = prompt
            .split("<character_spec>\n")
            .nth(1)
            .and_then(|rest| rest.split("\n</character_spec>").next())
            .unwrap();
        let expected = r##"{
  "elements": [
    {
      "category": "clothing",
      "description": "hat description",
      "id": "hat",
      "requirement": "mandatory"
    },
    {
      "category": "clothing",
      "description": "cloak description",
      "id": "cloak",
      "requirement": "mandatory"
    },
    {
      "category": "clothing",
      "description": "lantern description",
      "id": "lantern",
      "requirement": "flexible"
    }
  ],
  "palette": [
    {
      "hex": "#1f4e79",
      "name": "Sea blue",
      "role": null
    }
  ],
  "summary": "A lighthouse keeper"
}"##;
        assert_eq!(block, expected);
    }

    #[tokio::test]
    async fn unknown_ids_invalid_verdicts_and_gaps_are_reported_not_coerced() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let response = r#"{"elements":[{"id":"hat","verdict":"perfect","note":"x"},{"id":"mood","verdict":"met","note":"x"},{"id":"boots","verdict":"met","note":"x"},{"id":"lantern","verdict":"met","note":"Lit"}],"summary":"Partial"}"#;

        let (result, _) = spec_review(&fixture, &id, response).await;
        let review = result.unwrap().spec_review.unwrap();

        assert_eq!(
            review.dropped,
            [
                "element 'hat' dropped: invalid verdict 'perfect'",
                "unknown element ids dropped: mood, boots",
            ]
        );
        let rows: Vec<_> = review
            .checklist
            .iter()
            .map(|item| (item.id.as_str(), item.verdict, item.note.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("hat", SpecVerdict::Unclear, "not assessed by the model"),
                ("cloak", SpecVerdict::Unclear, "not assessed by the model"),
                ("lantern", SpecVerdict::Met, "Lit"),
            ]
        );
    }

    #[tokio::test]
    async fn duplicate_and_oversized_items_are_dropped() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let long = "n".repeat(501);
        let response = format!(
            r#"{{"elements":[{{"id":"hat","verdict":"met","note":"a"}},{{"id":"hat","verdict":"deviation","note":"b"}},{{"id":"cloak","verdict":"met","note":"{long}"}},{{"id":"lantern","verdict":"met","note":"ok"}}],"summary":"s"}}"#
        );

        let (result, _) = spec_review(&fixture, &id, &response).await;
        let review = result.unwrap().spec_review.unwrap();

        assert_eq!(
            review.dropped,
            [
                "element 'cloak' dropped: note is 501 bytes; the limit is 500 bytes",
                "element 'hat' dropped: listed 2 times",
            ]
        );
        assert_eq!(review.checklist[0].verdict, SpecVerdict::Unclear);
        assert_eq!(
            review.checklist[0].note,
            "conflicting duplicate entries; not recorded"
        );
        assert_eq!(review.checklist[1].verdict, SpecVerdict::Unclear);
        assert_eq!(review.checklist[1].note, "not assessed by the model");
        assert_eq!(review.checklist[2].id, "lantern");
    }

    #[tokio::test]
    async fn malformed_truncated_or_empty_checklists_store_nothing() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let long_summary = format!(
            r#"{{"elements":[{{"id":"hat","verdict":"met","note":"a"}}],"summary":"{}"}}"#,
            "s".repeat(1001)
        );
        let responses = [
            r#"{"elements":[{"id":"hat","verdict":"met","note":"a"}],"summa"#.to_owned(),
            r#"{"elements":[{"id":"hat","verdict":"met","note":"a"}],"summary":"s","extra":1}"#
                .to_owned(),
            r#"{"elements":"hat","summary":"s"}"#.to_owned(),
            r#"{"elements":[],"summary":"s"}"#.to_owned(),
            r#"{"elements":[{"id":"boots","verdict":"met","note":"a"}],"summary":"s"}"#.to_owned(),
            long_summary,
        ];
        for response in responses {
            let (result, _) = spec_review(&fixture, &id, &response).await;
            assert!(
                matches!(result, Err(HarnessError::VisionReviewResponse(_))),
                "{response}"
            );
        }
        assert!(stored(&fixture).await.is_empty());
    }

    #[tokio::test]
    async fn unknown_character_or_version_is_not_found_without_a_model_call() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let provider = FakeLlm::new("{}");
        let binding = vision_binding(Arc::new(provider.clone())).await;
        let service = VisionReviewService::new(fixture.harness.clone());

        let missing_character = service
            .review_against_spec(&binding, &fixture.output_id, "nobody", 1)
            .await;
        let missing_version = service
            .review_against_spec(&binding, &fixture.output_id, &id, 2)
            .await;
        let missing_output = service
            .review_against_spec(&binding, "review-run:9", &id, 1)
            .await;

        assert!(matches!(
            missing_character,
            Err(HarnessError::CharacterNotFound { .. })
        ));
        assert!(matches!(
            missing_version,
            Err(HarnessError::CharacterSpecVersionNotFound { version: 2, .. })
        ));
        assert!(matches!(missing_output, Err(HarnessError::OutputNotFound)));
        assert!(provider.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn spec_reviews_are_listed_with_vision_reviews_newest_first() {
        let fixture = fixture().await;
        let id = character(&fixture).await;
        let binding = vision_binding(Arc::new(FakeLlm::new(REVIEW_RESPONSE))).await;
        let service = VisionReviewService::new(fixture.harness.clone());
        let first = service
            .review_output(&binding, &fixture.output_id)
            .await
            .unwrap();
        let (second, _) = spec_review(
            &fixture,
            &id,
            r#"{"elements":[{"id":"hat","verdict":"met","note":"a"},{"id":"cloak","verdict":"met","note":"b"}],"summary":"s"}"#,
        )
        .await;
        let second = second.unwrap();

        let listed = stored(&fixture).await;

        assert_eq!(listed, vec![second, first]);
        assert_eq!(listed[1].spec_review, None);
    }

    async fn many_element_character(fixture: &Fixture, count: usize) -> String {
        let created = CharacterService::new(fixture.harness.clone())
            .create_character(
                NewCharacter {
                    name: "Crowd".into(),
                    spec: Some(CharacterSpec {
                        summary: "Many".into(),
                        elements: (0..count)
                            .map(|index| element(&format!("e{index}"), Requirement::Mandatory))
                            .collect(),
                        palette: vec![],
                        views: vec![],
                        style_notes: None,
                    }),
                    note: None,
                },
                None,
            )
            .await
            .unwrap();
        created.character.id
    }

    #[tokio::test]
    async fn adversarial_answers_are_capped_before_storage() {
        let fixture = fixture().await;
        let id = many_element_character(&fixture, 60).await;
        let item = |id: &str, verdict: &str| {
            format!(r#"{{"id":"{id}","verdict":"{verdict}","note":"x"}}"#)
        };
        let answer =
            |items: Vec<String>| format!(r#"{{"elements":[{}],"summary":"s"}}"#, items.join(","));

        let too_many = answer(
            (0..257)
                .map(|n| item(&format!("e{}", n % 60), "met"))
                .collect(),
        );
        let (result, _) = spec_review(&fixture, &id, &too_many).await;
        assert!(matches!(result, Err(HarnessError::VisionReviewResponse(_))));
        assert!(stored(&fixture).await.is_empty());

        let mut items = vec![item("e0", "met")];
        items.extend((1..60).map(|n| item(&format!("e{n}"), "bogus")));
        let (result, _) = spec_review(&fixture, &id, &answer(items)).await;
        let review = result.unwrap().spec_review.unwrap();
        assert_eq!(review.dropped.len(), 51);
        assert_eq!(review.dropped[50], "9 further items dropped");

        let mut items = vec![item("e0", "met")];
        items.extend((0..80).map(|n| item(&format!("{}{n}", "z".repeat(12_000)), "met")));
        let response = answer(items);
        assert!(response.len() > 900_000 && response.len() < 1_048_576);
        let (result, _) = spec_review(&fixture, &id, &response).await;
        let review = result.unwrap().spec_review.unwrap();
        assert_eq!(review.dropped.len(), 1);
        let unknown = &review.dropped[0];
        assert!(unknown.starts_with("unknown element ids dropped: "));
        assert!(unknown.ends_with(" and 60 more"), "{unknown}");
        assert!(unknown.len() < 20 * 70 + 100);
        assert!(serde_json::to_string(&review).unwrap().len() < 40_000);
        assert_eq!(stored(&fixture).await.len(), 2);
    }

    #[tokio::test]
    async fn a_spec_without_reviewable_elements_is_refused_before_the_model() {
        let fixture = fixture().await;
        let service = CharacterService::new(fixture.harness.clone());
        for elements in [vec![], vec![element("mood", Requirement::Unspecified)]] {
            let created = service
                .create_character(
                    NewCharacter {
                        name: "Blank".into(),
                        spec: Some(CharacterSpec {
                            summary: "s".into(),
                            elements,
                            palette: vec![],
                            views: vec![],
                            style_notes: None,
                        }),
                        note: None,
                    },
                    None,
                )
                .await
                .unwrap();
            let (result, provider) = spec_review(&fixture, &created.character.id, "{}").await;
            assert!(matches!(
                result,
                Err(HarnessError::SpecReviewNothingToReview)
            ));
            assert!(provider.requests.lock().unwrap().is_empty());
        }
        assert!(stored(&fixture).await.is_empty());
    }
}
