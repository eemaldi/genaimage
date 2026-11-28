use std::{io::Cursor, sync::Arc};

use chrono::Utc;
use image::ImageFormat;
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;
use crate::{
    async_trait,
    image_backend::{
        BackendHealth, EventMonitor, HistorySnapshot, OutputReference, PromptResponse,
        QueueSnapshot, UploadResponse,
    },
    media::OutputListQuery,
    persistence::{NewRun, OutputRecord, RunRepository},
    workflow::GenerationMode,
    Harness, HarnessConfig, ImageBackend, RunStatus,
};

struct NoBackend;

fn unused<T>() -> Result<T> {
    Err(HarnessError::UnsupportedOperation(
        "not used by character tests".to_owned(),
    ))
}

#[async_trait]
impl ImageBackend for NoBackend {
    fn name(&self) -> &str {
        "none"
    }
    fn base_url(&self) -> &str {
        "fake://local"
    }
    async fn health(&self) -> Result<BackendHealth> {
        unused()
    }
    async fn object_info(&self) -> Result<Value> {
        unused()
    }
    async fn queue(&self) -> Result<QueueSnapshot> {
        unused()
    }
    async fn submit(&self, _: &Value, _: &str, _: &str) -> Result<PromptResponse> {
        unused()
    }
    async fn history(&self, _: &str) -> Result<HistorySnapshot> {
        unused()
    }
    async fn delete_queued(&self, _: &str) -> Result<Value> {
        unused()
    }
    async fn interrupt(&self, _: &str) -> Result<Value> {
        unused()
    }
    async fn upload_image(&self, _: &std::path::Path) -> Result<UploadResponse> {
        unused()
    }
    async fn download_output(&self, _: &OutputReference) -> Result<Vec<u8>> {
        unused()
    }
    async fn monitor_events(&self, _: &str) -> Result<EventMonitor> {
        unused()
    }
}

struct Fixture {
    _directory: TempDir,
    config: crate::ResolvedConfig,
    harness: Harness,
    service: CharacterService,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let config =
        HarnessConfig::load_with(directory.path(), None, directory.path(), |key| match key {
            "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => {
                Some(directory.path().display().to_string())
            }
            _ => None,
        })
        .unwrap();
    let harness = Harness::new(config.clone(), Arc::new(NoBackend)).unwrap();
    Fixture {
        _directory: directory,
        service: harness.characters(),
        config,
        harness,
    }
}

fn spec(summary: &str) -> CharacterSpec {
    CharacterSpec {
        summary: summary.into(),
        elements: vec![SpecElement {
            id: "hat".into(),
            category: ElementCategory::Clothing,
            description: "Yellow sou'wester".into(),
            requirement: Requirement::Mandatory,
        }],
        palette: vec![PaletteEntry {
            name: "Oilskin".into(),
            hex: Some("#ffcc00".into()),
            role: None,
        }],
        views: vec![View::Front],
        style_notes: Some("Gouache".into()),
    }
}

async fn character(fixture: &Fixture) -> CreatedCharacter {
    fixture
        .service
        .create_character(NewCharacter {
            name: "Keeper".into(),
            spec: Some(spec("v1")),
            note: Some("first".into()),
        })
        .await
        .unwrap()
}

fn new_version(based_on: Option<u32>, summary: &str) -> NewSpecVersion {
    NewSpecVersion {
        based_on,
        spec: spec(summary),
        note: None,
    }
}

/// Inserts a run in the given status with one output and returns the output ID.
async fn output(fixture: &Fixture, run_id: &str, status: RunStatus) -> String {
    let repository = fixture.harness.repository().as_async();
    repository
        .insert(&NewRun {
            id: run_id.into(),
            prompt_id: format!("prompt-{run_id}"),
            client_id: "client".into(),
            workflow_id: "workflow".into(),
            workflow_version: "1".into(),
            workflow_sha256: "a".repeat(64),
            workflow_manifest_sha256: Some("b".repeat(64)),
            rerun_of: None,
            generation_mode: GenerationMode::Generic,
            status,
            parameters: json!({}),
            output_dir: format!("runs/{run_id}"),
        })
        .await
        .unwrap();
    repository
        .record_output(&OutputRecord {
            run_id: run_id.into(),
            node_id: "9".into(),
            kind: "images".into(),
            source_filename: "result.png".into(),
            source_subfolder: String::new(),
            source_type: "output".into(),
            local_path: format!("runs/{run_id}/result.png"),
            sha256: "c".repeat(64),
            byte_size: 1,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    repository
        .list_outputs(OutputListQuery {
            run: Some(run_id.into()),
            status: Some(status),
            ..OutputListQuery::default()
        })
        .await
        .unwrap()
        .outputs
        .remove(0)
        .id
}

async fn upload(fixture: &Fixture, shade: u8) -> String {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([shade, 0, 0])))
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let mut staged = fixture.harness.uploads().stage("ref.png").await.unwrap();
    staged.write_chunk(bytes.get_ref()).await.unwrap();
    staged.publish().await.unwrap().upload_id
}

async fn link_run(fixture: &Fixture, run_id: &str, link: &CharacterLink) {
    fixture
        .harness
        .repository()
        .as_async()
        .insert_owned_submission(
            NewRun {
                id: run_id.into(),
                prompt_id: format!("prompt-{run_id}"),
                client_id: "client".into(),
                workflow_id: "workflow".into(),
                workflow_version: "1".into(),
                workflow_sha256: "a".repeat(64),
                workflow_manifest_sha256: Some("b".repeat(64)),
                rerun_of: None,
                generation_mode: GenerationMode::Generic,
                status: RunStatus::Completed,
                parameters: json!({}),
                output_dir: format!("runs/{run_id}"),
            },
            Some(link.clone()),
        )
        .await
        .unwrap();
}

fn assert_invalid<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(HarnessError::InvalidCharacterRequest(message)) => message,
        other => panic!("expected an invalid character request, got {other:?}"),
    }
}

#[tokio::test]
async fn creating_a_character_writes_it_and_version_one_together() {
    let fixture = fixture();
    let created = character(&fixture).await;
    assert!(created.character.id.starts_with("char-"));
    assert_eq!(created.character.name, "Keeper");
    assert_eq!(created.character.latest_spec_version, Some(1));
    let version = created.spec_version.unwrap();
    assert_eq!((version.version, version.based_on), (1, None));
    assert_eq!(version.note.as_deref(), Some("first"));
    assert_eq!(
        fixture
            .service
            .get_character(&created.character.id)
            .await
            .unwrap(),
        created.character
    );

    let bare = fixture
        .service
        .create_character(NewCharacter {
            name: "Bare".into(),
            spec: None,
            note: None,
        })
        .await
        .unwrap();
    assert_eq!(bare.character.latest_spec_version, None);
    assert!(bare.spec_version.is_none());
    assert!(fixture
        .service
        .latest_spec_version(&bare.character.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn invalid_creation_writes_nothing() {
    let fixture = fixture();
    let new = |name: &str, spec: Option<CharacterSpec>, note: Option<&str>| NewCharacter {
        name: name.into(),
        spec,
        note: note.map(str::to_owned),
    };
    for request in [
        new("", None, None),
        new("   ", None, None),
        new(&"n".repeat(NAME_MAX_BYTES + 1), None, None),
        new("two\nlines", None, None),
        new("Keeper", None, Some("a note without a spec")),
        new("Keeper", Some(spec("  ")), None),
        new(
            "Keeper",
            Some(spec("ok")),
            Some(&"n".repeat(NOTE_MAX_BYTES + 1)),
        ),
    ] {
        assert_invalid(fixture.service.create_character(request).await);
    }
    let page = fixture
        .service
        .list_characters(CharacterListQuery::default())
        .await
        .unwrap();
    assert!(page.characters.is_empty());
}

#[tokio::test]
async fn spec_versions_are_append_only_and_conflicts_name_the_latest() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    let second = fixture
        .service
        .create_spec_version(id, new_version(Some(1), "v2"))
        .await
        .unwrap();
    assert_eq!((second.version, second.based_on), (2, Some(1)));
    assert_eq!(
        fixture
            .service
            .get_character(id)
            .await
            .unwrap()
            .latest_spec_version,
        Some(2)
    );
    // Version 1 is exactly as it was written.
    assert_eq!(
        fixture.service.get_spec_version(id, 1).await.unwrap(),
        created.spec_version.unwrap()
    );

    for based_on in [None, Some(1), Some(3)] {
        match fixture
            .service
            .create_spec_version(id, new_version(based_on, "rejected"))
            .await
        {
            Err(HarnessError::CharacterSpecConflict {
                character_id,
                based_on: reported,
                latest,
            }) => {
                assert_eq!(&character_id, id);
                assert_eq!((reported, latest), (based_on, Some(2)));
            }
            other => panic!("expected a conflict for {based_on:?}, got {other:?}"),
        }
    }
    let message = HarnessError::CharacterSpecConflict {
        character_id: id.clone(),
        based_on: Some(1),
        latest: Some(2),
    }
    .to_string();
    assert!(message.contains("nothing was written"), "{message}");
    assert!(fixture.service.get_spec_version(id, 3).await.is_err());

    let first_only = fixture
        .service
        .create_character(NewCharacter {
            name: "No spec yet".into(),
            spec: None,
            note: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .service
            .create_spec_version(&first_only.character.id, new_version(Some(1), "x"))
            .await,
        Err(HarnessError::CharacterSpecConflict {
            based_on: Some(1),
            latest: None,
            ..
        })
    ));
    let first = fixture
        .service
        .create_spec_version(&first_only.character.id, new_version(None, "first"))
        .await
        .unwrap();
    assert_eq!(first.version, 1);
}

#[tokio::test]
async fn spec_validation_rejects_before_anything_is_written() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    let mut duplicate = spec("dup");
    duplicate.elements.push(duplicate.elements[0].clone());
    let message = assert_invalid(
        fixture
            .service
            .create_spec_version(
                id,
                NewSpecVersion {
                    based_on: Some(1),
                    spec: duplicate,
                    note: None,
                },
            )
            .await,
    );
    assert!(message.contains("unique"), "{message}");
    let mut bad_hex = spec("hex");
    bad_hex.palette[0].hex = Some("ffcc00".into());
    assert_invalid(
        fixture
            .service
            .create_spec_version(
                id,
                NewSpecVersion {
                    based_on: Some(1),
                    spec: bad_hex,
                    note: None,
                },
            )
            .await,
    );
    assert_eq!(
        fixture
            .service
            .get_character(id)
            .await
            .unwrap()
            .latest_spec_version,
        Some(1)
    );
}

#[tokio::test]
async fn spec_versions_list_newest_first_with_a_cursor() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    for version in 2..=3 {
        fixture
            .service
            .create_spec_version(id, new_version(Some(version - 1), &format!("v{version}")))
            .await
            .unwrap();
    }
    let first = fixture
        .service
        .list_spec_versions(
            id,
            SpecVersionListQuery {
                limit: Some(2),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        first.versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        [3, 2]
    );
    let rest = fixture
        .service
        .list_spec_versions(
            id,
            SpecVersionListQuery {
                limit: Some(2),
                cursor: first.next_cursor,
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.versions.len(), 1);
    assert_eq!(rest.versions[0].version, 1);
    assert!(rest.next_cursor.is_none());
}

#[test]
fn concurrent_creators_on_separate_connections_have_exactly_one_winner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite3");
    let repository = RunRepository::open(&path).unwrap();
    let (character, _) = repository
        .create_character("Racing", Some((&spec("v1"), None)))
        .unwrap();
    const CREATORS: usize = 8;
    let barrier = std::sync::Barrier::new(CREATORS);
    let outcomes: Vec<Result<CharacterSpecVersion>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..CREATORS)
            .map(|index| {
                let (barrier, path, id) = (&barrier, &path, &character.id);
                scope.spawn(move || {
                    let repository = RunRepository::open(path).unwrap();
                    barrier.wait();
                    repository.create_spec_version(
                        id,
                        Some(1),
                        &spec(&format!("racer {index}")),
                        None,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let winners: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .collect();
    assert_eq!(winners.len(), 1, "{outcomes:?}");
    assert_eq!(winners[0].version, 2);
    for outcome in &outcomes {
        if let Err(error) = outcome {
            assert!(
                matches!(
                    error,
                    HarnessError::CharacterSpecConflict {
                        based_on: Some(1),
                        latest: Some(2),
                        ..
                    }
                ),
                "{error:?}"
            );
        }
    }
    let stored = repository.get_spec_version(&character.id, 2).unwrap();
    assert_eq!(stored.spec, winners[0].spec);
    assert!(repository.get_spec_version(&character.id, 3).is_err());
}

#[tokio::test]
async fn unknown_characters_are_rejected_by_every_operation() {
    let fixture = fixture();
    let missing = "char-missing";
    let service = &fixture.service;
    macro_rules! not_found {
        ($call:expr) => {
            assert!(
                matches!(
                    $call.await,
                    Err(HarnessError::CharacterNotFound { ref character_id }) if character_id == missing
                ),
                "{}",
                stringify!($call)
            )
        };
    }
    not_found!(service.get_character(missing));
    not_found!(service.get_spec_version(missing, 1));
    not_found!(service.latest_spec_version(missing));
    not_found!(service.list_spec_versions(missing, Default::default()));
    not_found!(service.create_spec_version(missing, new_version(None, "x")));
    not_found!(service.remove_reference(missing, "charref-x"));
    not_found!(service.list_references(missing, Default::default()));
    not_found!(service.list_decisions(missing, Default::default()));
    not_found!(service.list_current_states(missing, Default::default()));
    not_found!(service.list_approved_outputs(missing, Default::default()));
    not_found!(service.current_state(missing, "run:1"));
    not_found!(service.list_runs(missing, Default::default()));
    not_found!(service.list_outputs(missing, Default::default()));
    not_found!(service.add_reference(
        missing,
        NewReference {
            upload_id: format!("sha256:{}", "0".repeat(64)),
            role: ReferenceRole::Sketch,
            note: None,
        },
    ));
    not_found!(service.record_decision(
        missing,
        NewDecision {
            output_id: "run:1".into(),
            spec_version: 1,
            decision: Decision::Approved,
            note: None,
        },
    ));
}

#[tokio::test]
async fn a_reference_needs_an_existing_upload_and_removal_keeps_history() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    let add = |upload_id: String, role| NewReference {
        upload_id,
        role,
        note: Some("front sketch".into()),
    };

    for unknown in [
        format!("sha256:{}", "0".repeat(64)),
        "not-an-upload".to_owned(),
        "sha256:".repeat(100),
    ] {
        assert!(fixture
            .service
            .add_reference(id, add(unknown, ReferenceRole::Sketch))
            .await
            .is_err());
    }
    assert!(fixture
        .service
        .list_references(
            id,
            ReferenceListQuery {
                include_removed: true,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .references
        .is_empty());

    let upload_id = upload(&fixture, 10).await;
    let sketch = fixture
        .service
        .add_reference(id, add(upload_id.clone(), ReferenceRole::Sketch))
        .await
        .unwrap();
    assert!(sketch.id.starts_with("charref-"));
    assert_eq!(sketch.role, ReferenceRole::Sketch);
    assert!(sketch.removed_at.is_none());
    // The same upload may serve another role, but not the same role twice.
    fixture
        .service
        .add_reference(id, add(upload_id.clone(), ReferenceRole::Mask))
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .service
            .add_reference(id, add(upload_id.clone(), ReferenceRole::Sketch))
            .await,
        Err(HarnessError::CharacterConflict(message)) if message.contains(&sketch.id)
    ));

    let removed = fixture
        .service
        .remove_reference(id, &sketch.id)
        .await
        .unwrap();
    assert!(removed.removed_at.is_some());
    assert_eq!(
        CharacterReference {
            removed_at: None,
            ..removed.clone()
        },
        sketch
    );
    // Removal is recorded once; repeating it changes nothing.
    assert_eq!(
        fixture
            .service
            .remove_reference(id, &sketch.id)
            .await
            .unwrap(),
        removed
    );
    let active = fixture
        .service
        .list_references(id, ReferenceListQuery::default())
        .await
        .unwrap();
    assert_eq!(active.references.len(), 1);
    assert_eq!(active.references[0].role, ReferenceRole::Mask);
    let all = fixture
        .service
        .list_references(
            id,
            ReferenceListQuery {
                include_removed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all.references.len(), 2);
    assert!(all.references.iter().any(|reference| reference == &removed));

    // A removed role can be attached again as a new reference.
    let again = fixture
        .service
        .add_reference(id, add(upload_id, ReferenceRole::Sketch))
        .await
        .unwrap();
    assert_ne!(again.id, sketch.id);

    let other = fixture
        .service
        .create_character(NewCharacter {
            name: "Other".into(),
            spec: None,
            note: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .service
            .remove_reference(&other.character.id, &again.id)
            .await,
        Err(HarnessError::CharacterReferenceNotFound { .. })
    ));
}

#[tokio::test]
async fn active_references_are_bounded() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    for shade in 0..MAX_ACTIVE_REFERENCES as u8 {
        let upload_id = upload(&fixture, shade).await;
        fixture
            .service
            .add_reference(
                id,
                NewReference {
                    upload_id,
                    role: ReferenceRole::Reference,
                    note: None,
                },
            )
            .await
            .unwrap();
    }
    let extra = upload(&fixture, 200).await;
    let message = assert_invalid(
        fixture
            .service
            .add_reference(
                id,
                NewReference {
                    upload_id: extra,
                    role: ReferenceRole::Reference,
                    note: None,
                },
            )
            .await,
    );
    assert!(message.contains("Nothing was written"), "{message}");
}

#[tokio::test]
async fn decisions_are_append_only_and_clearing_is_the_undo() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    fixture
        .service
        .create_spec_version(id, new_version(Some(1), "v2"))
        .await
        .unwrap();
    let output = output(&fixture, "run-a", RunStatus::Completed).await;
    let decide = |decision, spec_version| NewDecision {
        output_id: output.clone(),
        spec_version,
        decision,
        note: None,
    };

    assert!(fixture
        .service
        .current_state(id, &output)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        fixture
            .service
            .record_decision(id, decide(Decision::Cleared, 1))
            .await,
        Err(HarnessError::CharacterConflict(_))
    ));

    let approved = fixture
        .service
        .record_decision(id, decide(Decision::Approved, 1))
        .await
        .unwrap();
    assert_eq!(
        fixture.service.current_state(id, &output).await.unwrap(),
        Some(approved.clone())
    );
    let rejected = fixture
        .service
        .record_decision(id, decide(Decision::Rejected, 2))
        .await
        .unwrap();
    assert_eq!(rejected.spec_version, 2);
    assert_eq!(
        fixture.service.current_state(id, &output).await.unwrap(),
        Some(rejected.clone())
    );
    let cleared = fixture
        .service
        .record_decision(id, decide(Decision::Cleared, 2))
        .await
        .unwrap();
    assert!(fixture
        .service
        .current_state(id, &output)
        .await
        .unwrap()
        .is_none());

    let history = fixture
        .service
        .list_decisions(id, DecisionListQuery::default())
        .await
        .unwrap();
    assert_eq!(
        history.decisions,
        [cleared.clone(), rejected, approved.clone()]
    );
    assert!(fixture
        .service
        .list_current_states(id, CurrentStateQuery::default())
        .await
        .unwrap()
        .decisions
        .is_empty());

    // Approving again after the undo is a new decision, never a revival of the old one.
    let again = fixture
        .service
        .record_decision(id, decide(Decision::Approved, 2))
        .await
        .unwrap();
    assert_ne!(again.id, approved.id);
    let approved_now = fixture
        .service
        .list_approved_outputs(id, CurrentStateQuery::default())
        .await
        .unwrap();
    assert_eq!(approved_now.decisions, [again]);
    assert_eq!(
        fixture
            .service
            .list_decisions(
                id,
                DecisionListQuery {
                    output_id: Some(output.clone()),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .decisions
            .len(),
        4
    );
}

#[tokio::test]
async fn current_states_take_the_latest_decision_per_output_and_filter() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    fixture
        .service
        .create_spec_version(id, new_version(Some(1), "v2"))
        .await
        .unwrap();
    let mut outputs = Vec::new();
    for index in 0..4 {
        outputs.push(output(&fixture, &format!("run-{index}"), RunStatus::Completed).await);
    }
    let decide = |output: &String, decision, spec_version| {
        fixture.service.record_decision(
            id,
            NewDecision {
                output_id: output.clone(),
                spec_version,
                decision,
                note: None,
            },
        )
    };
    decide(&outputs[0], Decision::Approved, 1).await.unwrap();
    decide(&outputs[1], Decision::Rejected, 1).await.unwrap();
    decide(&outputs[2], Decision::Approved, 2).await.unwrap();
    decide(&outputs[2], Decision::Rejected, 2).await.unwrap();
    decide(&outputs[3], Decision::Approved, 2).await.unwrap();
    decide(&outputs[3], Decision::Cleared, 2).await.unwrap();

    let ids = |page: DecisionPage| -> Vec<(String, Decision)> {
        page.decisions
            .into_iter()
            .map(|d| (d.output_id, d.decision))
            .collect()
    };
    let all = ids(fixture
        .service
        .list_current_states(id, CurrentStateQuery::default())
        .await
        .unwrap());
    assert_eq!(
        all,
        [
            (outputs[2].clone(), Decision::Rejected),
            (outputs[1].clone(), Decision::Rejected),
            (outputs[0].clone(), Decision::Approved),
        ]
    );
    let approved = ids(fixture
        .service
        .list_approved_outputs(id, CurrentStateQuery::default())
        .await
        .unwrap());
    assert_eq!(approved, [(outputs[0].clone(), Decision::Approved)]);
    let at_two = ids(fixture
        .service
        .list_current_states(
            id,
            CurrentStateQuery {
                spec_version: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap());
    assert_eq!(at_two, [(outputs[2].clone(), Decision::Rejected)]);

    let first = fixture
        .service
        .list_current_states(
            id,
            CurrentStateQuery {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.decisions.len(), 2);
    let rest = fixture
        .service
        .list_current_states(
            id,
            CurrentStateQuery {
                limit: Some(2),
                cursor: first.next_cursor,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.decisions.len(), 1);
    assert!(rest.next_cursor.is_none());

    assert_invalid(
        fixture
            .service
            .list_current_states(
                id,
                CurrentStateQuery {
                    state: Some(Decision::Cleared),
                    ..Default::default()
                },
            )
            .await,
    );
}

#[tokio::test]
async fn decisions_need_an_existing_version_and_output_of_a_completed_run() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    let completed = output(&fixture, "run-ok", RunStatus::Completed).await;
    let failed = output(&fixture, "run-failed", RunStatus::Failed).await;
    let decide = |output_id: &str, spec_version| NewDecision {
        output_id: output_id.into(),
        spec_version,
        decision: Decision::Approved,
        note: None,
    };
    assert!(matches!(
        fixture
            .service
            .record_decision(id, decide(&completed, 2))
            .await,
        Err(HarnessError::CharacterSpecVersionNotFound { version: 2, .. })
    ));
    assert!(matches!(
        fixture
            .service
            .record_decision(id, decide(&completed, 0))
            .await,
        Err(HarnessError::CharacterSpecVersionNotFound { version: 0, .. })
    ));
    for unknown in ["run-ok:999", "missing:1", "no-colon", "run-ok:x"] {
        assert!(matches!(
            fixture
                .service
                .record_decision(id, decide(unknown, 1))
                .await,
            Err(HarnessError::OutputNotFound)
        ));
    }
    let message = assert_invalid(
        fixture
            .service
            .record_decision(id, decide(&failed, 1))
            .await,
    );
    assert!(
        message.contains("failed") && message.contains("Nothing was written"),
        "{message}"
    );
    assert_invalid(
        fixture
            .service
            .record_decision(id, decide(&"o".repeat(OUTPUT_ID_MAX_BYTES + 1), 1))
            .await,
    );
    assert!(fixture
        .service
        .list_decisions(id, DecisionListQuery::default())
        .await
        .unwrap()
        .decisions
        .is_empty());
}

#[tokio::test]
async fn runs_and_outputs_list_by_character_with_a_cursor() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = &created.character.id;
    fixture
        .service
        .create_spec_version(id, new_version(Some(1), "v2"))
        .await
        .unwrap();
    for (run, version) in [("run-1", 1), ("run-2", 2), ("run-3", 2)] {
        link_run(
            &fixture,
            run,
            &CharacterLink {
                id: id.clone(),
                spec_version: version,
            },
        )
        .await;
        fixture
            .harness
            .repository()
            .as_async()
            .record_output(&OutputRecord {
                run_id: run.into(),
                node_id: "9".into(),
                kind: "images".into(),
                source_filename: "result.png".into(),
                source_subfolder: String::new(),
                source_type: "output".into(),
                local_path: format!("runs/{run}/result.png"),
                sha256: "c".repeat(64),
                byte_size: 1,
                created_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    output(&fixture, "run-unlinked", RunStatus::Completed).await;

    let runs = |query| fixture.service.list_runs(id, query);
    let all = runs(CharacterRunQuery::default()).await.unwrap();
    let mut listed: Vec<_> = all.runs.iter().map(|run| run.id.as_str()).collect();
    listed.sort_unstable();
    assert_eq!(listed, ["run-1", "run-2", "run-3"]);
    assert!(all
        .runs
        .iter()
        .all(|run| run.character.as_ref().unwrap().id == *id));
    let v2 = runs(CharacterRunQuery {
        spec_version: Some(2),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(v2.runs.len(), 2);
    assert!(runs(CharacterRunQuery {
        status: Some(RunStatus::Failed),
        ..Default::default()
    })
    .await
    .unwrap()
    .runs
    .is_empty());

    let first = runs(CharacterRunQuery {
        limit: Some(2),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(first.runs.len(), 2);
    let rest = runs(CharacterRunQuery {
        limit: Some(2),
        cursor: first.next_cursor.clone(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(rest.runs.len(), 1);
    assert!(rest.next_cursor.is_none());
    let mut seen: Vec<_> = first
        .runs
        .iter()
        .chain(&rest.runs)
        .map(|run| &run.id)
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3);

    let outputs = fixture
        .service
        .list_outputs(id, CharacterRunQuery::default())
        .await
        .unwrap();
    assert_eq!(outputs.outputs.len(), 3);
    let page = fixture
        .service
        .list_outputs(
            id,
            CharacterRunQuery {
                spec_version: Some(2),
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.outputs.len(), 1);
    assert!(page.next_cursor.is_some());
}

#[tokio::test]
async fn a_run_cannot_link_an_unknown_character_or_version() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let repository = fixture.harness.repository().as_async();
    for link in [
        CharacterLink {
            id: "char-missing".into(),
            spec_version: 1,
        },
        CharacterLink {
            id: created.character.id.clone(),
            spec_version: 2,
        },
    ] {
        assert!(repository
            .verify_character_link(link.clone())
            .await
            .is_err());
        let result = repository
            .insert_owned_submission(
                NewRun {
                    id: "run-bad".into(),
                    prompt_id: "prompt-bad".into(),
                    client_id: "client".into(),
                    workflow_id: "workflow".into(),
                    workflow_version: "1".into(),
                    workflow_sha256: "a".repeat(64),
                    workflow_manifest_sha256: None,
                    rerun_of: None,
                    generation_mode: GenerationMode::Generic,
                    status: RunStatus::Submitting,
                    parameters: json!({}),
                    output_dir: "runs/bad".into(),
                },
                Some(link),
            )
            .await;
        assert!(result.is_err());
        // The run and its link are written together or not at all.
        assert!(repository.get("run-bad").await.is_err());
    }
}

#[tokio::test]
async fn limits_and_cursors_are_validated() {
    let fixture = fixture();
    for limit in [0, MAX_PAGE_LIMIT + 1] {
        assert_invalid(
            fixture
                .service
                .list_characters(CharacterListQuery {
                    limit: Some(limit),
                    cursor: None,
                })
                .await,
        );
    }
    for cursor in [
        "",
        "v1.",
        "v1.zz",
        "v2.31",
        "v1.2d31",
        "v1.30",
        &"v1.31".repeat(20),
    ] {
        assert_invalid(
            fixture
                .service
                .list_characters(CharacterListQuery {
                    limit: None,
                    cursor: Some(cursor.to_owned()),
                })
                .await,
        );
    }
}

#[tokio::test]
async fn characters_list_newest_first_and_page() {
    let fixture = fixture();
    let mut ids = Vec::new();
    for name in ["A", "B", "C"] {
        ids.push(
            fixture
                .service
                .create_character(NewCharacter {
                    name: name.into(),
                    spec: None,
                    note: None,
                })
                .await
                .unwrap()
                .character
                .id,
        );
    }
    let first = fixture
        .service
        .list_characters(CharacterListQuery {
            limit: Some(2),
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(
        first
            .characters
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        [ids[2].clone(), ids[1].clone()]
    );
    let rest = fixture
        .service
        .list_characters(CharacterListQuery {
            limit: Some(2),
            cursor: first.next_cursor,
        })
        .await
        .unwrap();
    assert_eq!(rest.characters.len(), 1);
    assert_eq!(rest.characters[0].id, ids[0]);
    assert!(rest.next_cursor.is_none());
}

#[tokio::test]
async fn everything_survives_reopening_the_database() {
    let fixture = fixture();
    let created = character(&fixture).await;
    let id = created.character.id.clone();
    fixture
        .service
        .create_spec_version(&id, new_version(Some(1), "v2"))
        .await
        .unwrap();
    let upload_id = upload(&fixture, 7).await;
    let kept = fixture
        .service
        .add_reference(
            &id,
            NewReference {
                upload_id: upload_id.clone(),
                role: ReferenceRole::Reference,
                note: None,
            },
        )
        .await
        .unwrap();
    let removed = fixture
        .service
        .add_reference(
            &id,
            NewReference {
                upload_id,
                role: ReferenceRole::Mask,
                note: Some("mask".into()),
            },
        )
        .await
        .unwrap();
    let removed = fixture
        .service
        .remove_reference(&id, &removed.id)
        .await
        .unwrap();
    let approved_output = output(&fixture, "run-1", RunStatus::Completed).await;
    let cleared_output = output(&fixture, "run-2", RunStatus::Completed).await;
    for (output_id, decision) in [
        (&approved_output, Decision::Approved),
        (&cleared_output, Decision::Rejected),
        (&cleared_output, Decision::Cleared),
    ] {
        fixture
            .service
            .record_decision(
                &id,
                NewDecision {
                    output_id: output_id.clone(),
                    spec_version: 2,
                    decision,
                    note: None,
                },
            )
            .await
            .unwrap();
    }
    let link = CharacterLink {
        id: id.clone(),
        spec_version: 2,
    };
    link_run(&fixture, "run-linked", &link).await;

    async fn snapshot(service: &CharacterService, id: &str) -> Value {
        json!({
            "character": service.get_character(id).await.unwrap(),
            "versions": service.list_spec_versions(id, Default::default()).await.unwrap(),
            "references": service.list_references(id, ReferenceListQuery {
                include_removed: true, ..Default::default()
            }).await.unwrap(),
            "decisions": service.list_decisions(id, Default::default()).await.unwrap(),
            "current": service.list_current_states(id, Default::default()).await.unwrap(),
            "runs": service.list_runs(id, Default::default()).await.unwrap().runs
                .into_iter().map(|run| run.id).collect::<Vec<_>>(),
        })
    }
    let before = snapshot(&fixture.service, &id).await;

    let Fixture {
        _directory,
        config,
        harness,
        service,
    } = fixture;
    drop((service, harness));
    let reopened = Harness::new(config, Arc::new(NoBackend))
        .unwrap()
        .characters();
    assert_eq!(snapshot(&reopened, &id).await, before);

    let character = reopened.get_character(&id).await.unwrap();
    assert_eq!(character.latest_spec_version, Some(2));
    let references = reopened
        .list_references(
            &id,
            ReferenceListQuery {
                include_removed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .references;
    assert!(references.contains(&kept) && references.contains(&removed));
    assert!(removed.removed_at.is_some());
    let current = reopened
        .list_current_states(&id, Default::default())
        .await
        .unwrap();
    assert_eq!(current.decisions.len(), 1);
    assert_eq!(current.decisions[0].output_id, approved_output);
    assert!(reopened
        .current_state(&id, &cleared_output)
        .await
        .unwrap()
        .is_none());
    let linked = reopened.list_runs(&id, Default::default()).await.unwrap();
    assert_eq!(linked.runs[0].character.as_ref(), Some(&link));
}
