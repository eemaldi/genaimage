use serde::{Deserialize, Serialize};

use super::{
    validate_name, validate_note, validate_output_id, Character, CharacterReference, CharacterSpec,
    CharacterSpecVersion, Decision, IdempotencyKey, OutputDecision, ReferenceRole,
};
use crate::{
    media::{IndexedOutput, OutputPage},
    persistence::{
        characters::{CurrentFilter, Keyed, Operation},
        RunPage,
    },
    Harness, HarnessError, Result, RunStatus,
};

fn keyed(
    key: Option<&IdempotencyKey>,
    operation: Operation,
    character_id: Option<&str>,
    request: &impl Serialize,
) -> Result<Option<Keyed>> {
    key.map(|key| {
        Keyed::new(
            key.as_str(),
            operation,
            serde_json::json!({ "character_id": character_id, "request": request }),
        )
    })
    .transpose()
}

const UPLOAD_ID_MAX_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewCharacter {
    pub name: String,
    /// Written as version 1 in the same transaction as the character.
    #[serde(default)]
    pub spec: Option<CharacterSpec>,
    /// Recorded with version 1; rejected when there is no spec.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreatedCharacter {
    pub character: Character,
    pub spec_version: Option<CharacterSpecVersion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewSpecVersion {
    /// The latest version the author saw; `None` only for the first version.
    #[serde(default)]
    pub based_on: Option<u32>,
    pub spec: CharacterSpec,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewReference {
    pub upload_id: String,
    pub role: ReferenceRole,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewDecision {
    pub output_id: String,
    /// The spec version the author judged the output against.
    pub spec_version: u32,
    pub decision: Decision,
    #[serde(default)]
    pub note: Option<String>,
}

/// Lists are newest first. A page without `next_cursor` is the last one.
#[derive(Clone, Debug, Default)]
pub struct CharacterListQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

pub type SpecVersionListQuery = CharacterListQuery;

#[derive(Clone, Debug, Default)]
pub struct ReferenceListQuery {
    pub include_removed: bool,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DecisionListQuery {
    pub output_id: Option<String>,
    pub spec_version: Option<u32>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

/// Current states are the latest decision per output, excluding cleared ones.
#[derive(Clone, Debug, Default)]
pub struct CurrentStateQuery {
    /// Keep only `approved` or `rejected` states. `cleared` is not a current state.
    pub state: Option<Decision>,
    /// Keep states decided against this spec version.
    pub spec_version: Option<u32>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CharacterRunQuery {
    pub spec_version: Option<u32>,
    /// Runs only; outputs always come from completed runs.
    pub status: Option<RunStatus>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CharacterPage {
    pub characters: Vec<Character>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpecVersionPage {
    pub versions: Vec<CharacterSpecVersion>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReferencePage {
    pub references: Vec<CharacterReference>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DecisionPage {
    pub decisions: Vec<OutputDecision>,
    pub next_cursor: Option<String>,
}

/// A decision with the indexed output it is about.
pub struct DecisionWithOutput {
    pub decision: OutputDecision,
    pub output: IndexedOutput,
}

pub struct DecisionOutputPage {
    pub decisions: Vec<DecisionWithOutput>,
    pub next_cursor: Option<String>,
}

/// Characters, their versioned specs, references and output decisions.
///
/// Every write is one transaction: when a method returns an error, nothing was
/// written, and the message says so where that is not obvious.
///
/// Every write takes an optional `IdempotencyKey`. The key is checked and
/// recorded in the write's own transaction: a repeat of the same request
/// returns the record written the first time and writes nothing, and the same
/// key with another request is `IdempotencyConflict`. A failed write records
/// no key, so a retry after a failure is evaluated afresh.
#[derive(Clone)]
pub struct CharacterService {
    harness: Harness,
}

impl CharacterService {
    pub fn new(harness: Harness) -> Self {
        Self { harness }
    }

    fn repository(&self) -> crate::persistence::AsyncRunRepository {
        self.harness.repository().as_async()
    }

    /// Creates the character and, when a spec is given, its version 1 together.
    pub async fn create_character(
        &self,
        request: NewCharacter,
        key: Option<&IdempotencyKey>,
    ) -> Result<CreatedCharacter> {
        validate_name(&request.name)?;
        validate_note("note", request.note.as_deref())?;
        match (&request.spec, &request.note) {
            (Some(spec), _) => spec.validate()?,
            (None, Some(_)) => {
                return Err(HarnessError::InvalidCharacterRequest(
                    "A note belongs to a spec version; give a spec or omit the note.".into(),
                ))
            }
            (None, None) => {}
        }
        let keyed = keyed(key, Operation::CreateCharacter, None, &request)?;
        let first = request.spec.map(|spec| (spec, request.note));
        let (character, spec_version) = self
            .repository()
            .create_character(request.name, first, keyed)
            .await?;
        Ok(CreatedCharacter {
            character,
            spec_version,
        })
    }

    pub async fn get_character(&self, character_id: &str) -> Result<Character> {
        self.repository()
            .get_character(character_id.to_owned())
            .await
    }

    pub async fn list_characters(&self, query: CharacterListQuery) -> Result<CharacterPage> {
        let (characters, next_cursor) = self
            .repository()
            .list_characters(query.limit, query.cursor)
            .await?;
        Ok(CharacterPage {
            characters,
            next_cursor,
        })
    }

    /// Appends a version. `based_on` must equal the current latest version (or be
    /// `None` for the first), otherwise `CharacterSpecConflict` reports the
    /// latest and nothing is written.
    pub async fn create_spec_version(
        &self,
        character_id: &str,
        request: NewSpecVersion,
        key: Option<&IdempotencyKey>,
    ) -> Result<CharacterSpecVersion> {
        request.spec.validate()?;
        validate_note("note", request.note.as_deref())?;
        let keyed = keyed(
            key,
            Operation::CreateSpecVersion,
            Some(character_id),
            &request,
        )?;
        self.repository()
            .create_spec_version(
                character_id.to_owned(),
                request.based_on,
                request.spec,
                request.note,
                keyed,
            )
            .await
    }

    pub async fn get_spec_version(
        &self,
        character_id: &str,
        version: u32,
    ) -> Result<CharacterSpecVersion> {
        self.repository()
            .get_spec_version(character_id.to_owned(), version)
            .await
    }

    pub async fn latest_spec_version(
        &self,
        character_id: &str,
    ) -> Result<Option<CharacterSpecVersion>> {
        self.repository()
            .latest_spec_version(character_id.to_owned())
            .await
    }

    pub async fn list_spec_versions(
        &self,
        character_id: &str,
        query: SpecVersionListQuery,
    ) -> Result<SpecVersionPage> {
        let (versions, next_cursor) = self
            .repository()
            .list_spec_versions(character_id.to_owned(), query.limit, query.cursor)
            .await?;
        Ok(SpecVersionPage {
            versions,
            next_cursor,
        })
    }

    /// Attaches an existing upload. The upload store is checked first, so an
    /// unknown or corrupt upload writes nothing.
    pub async fn add_reference(
        &self,
        character_id: &str,
        request: NewReference,
        key: Option<&IdempotencyKey>,
    ) -> Result<CharacterReference> {
        validate_note("note", request.note.as_deref())?;
        if request.upload_id.len() > UPLOAD_ID_MAX_BYTES {
            return Err(HarnessError::InvalidCharacterRequest(format!(
                "upload_id must be at most {UPLOAD_ID_MAX_BYTES} bytes."
            )));
        }
        let keyed = keyed(key, Operation::AddReference, Some(character_id), &request)?;
        // A repeat replays the first outcome even if the upload store changed since.
        let repeated = match key {
            Some(key) => self.repository().has_claim(key.as_str().to_owned()).await?,
            None => false,
        };
        if !repeated {
            self.repository()
                .get_character(character_id.to_owned())
                .await?;
            if self
                .harness
                .uploads()
                .metadata(&request.upload_id)
                .await?
                .is_none()
            {
                return Err(HarnessError::CharacterUploadNotFound {
                    upload_id: request.upload_id,
                });
            }
        }
        self.repository()
            .add_reference(
                character_id.to_owned(),
                request.upload_id,
                request.role,
                request.note,
                keyed,
            )
            .await
    }

    /// A soft removal: the row stays, with `removed_at` set once. Removing a
    /// reference that is already removed returns it unchanged.
    pub async fn remove_reference(
        &self,
        character_id: &str,
        reference_id: &str,
        key: Option<&IdempotencyKey>,
    ) -> Result<CharacterReference> {
        let keyed = keyed(
            key,
            Operation::RemoveReference,
            Some(character_id),
            &reference_id,
        )?;
        self.repository()
            .remove_reference(character_id.to_owned(), reference_id.to_owned(), keyed)
            .await
    }

    pub async fn list_references(
        &self,
        character_id: &str,
        query: ReferenceListQuery,
    ) -> Result<ReferencePage> {
        let (references, next_cursor) = self
            .repository()
            .list_references(
                character_id.to_owned(),
                query.include_removed,
                query.limit,
                query.cursor,
            )
            .await?;
        Ok(ReferencePage {
            references,
            next_cursor,
        })
    }

    /// Appends a decision. The output must exist and, to be approved or
    /// rejected, belong to a completed run; the spec version must exist. A run
    /// submitted against another character is refused; an unlinked run is
    /// allowed. A `cleared` decision needs a current decision to undo, whatever
    /// spec version the earlier decision was made against.
    pub async fn record_decision(
        &self,
        character_id: &str,
        request: NewDecision,
        key: Option<&IdempotencyKey>,
    ) -> Result<OutputDecision> {
        validate_output_id(&request.output_id)?;
        validate_note("note", request.note.as_deref())?;
        let keyed = keyed(key, Operation::RecordDecision, Some(character_id), &request)?;
        self.repository()
            .record_decision(
                character_id.to_owned(),
                request.output_id,
                request.spec_version,
                request.decision,
                request.note,
                keyed,
            )
            .await
    }

    /// The latest decision for an output, or `None` if it has none or was cleared.
    pub async fn current_state(
        &self,
        character_id: &str,
        output_id: &str,
    ) -> Result<Option<OutputDecision>> {
        validate_output_id(output_id)?;
        self.repository()
            .current_decision(character_id.to_owned(), output_id.to_owned())
            .await
    }

    /// Every decision, including `cleared` ones, newest first.
    pub async fn list_decisions(
        &self,
        character_id: &str,
        query: DecisionListQuery,
    ) -> Result<DecisionPage> {
        if let Some(output_id) = &query.output_id {
            validate_output_id(output_id)?;
        }
        self.decisions(
            character_id,
            query.output_id,
            query.spec_version,
            None,
            query.limit,
            query.cursor,
        )
        .await
    }

    pub async fn list_current_states(
        &self,
        character_id: &str,
        query: CurrentStateQuery,
    ) -> Result<DecisionPage> {
        let filter = current_filter(query.state)?;
        self.decisions(
            character_id,
            None,
            query.spec_version,
            Some(filter),
            query.limit,
            query.cursor,
        )
        .await
    }

    pub async fn list_approved_outputs(
        &self,
        character_id: &str,
        query: CurrentStateQuery,
    ) -> Result<DecisionPage> {
        self.list_current_states(
            character_id,
            CurrentStateQuery {
                state: Some(Decision::Approved),
                ..query
            },
        )
        .await
    }

    async fn decisions(
        &self,
        character_id: &str,
        output_id: Option<String>,
        spec_version: Option<u32>,
        current: Option<CurrentFilter>,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<DecisionPage> {
        let (decisions, next_cursor) = self
            .repository()
            .list_decisions(
                character_id.to_owned(),
                output_id,
                spec_version,
                current,
                limit,
                cursor,
            )
            .await?;
        Ok(DecisionPage {
            decisions,
            next_cursor,
        })
    }

    /// `list_decisions` with each decision's indexed output, in one read.
    pub async fn list_decisions_with_outputs(
        &self,
        character_id: &str,
        query: DecisionListQuery,
    ) -> Result<DecisionOutputPage> {
        if let Some(output_id) = &query.output_id {
            validate_output_id(output_id)?;
        }
        self.decisions_with_outputs(
            character_id,
            query.output_id,
            query.spec_version,
            None,
            query.limit,
            query.cursor,
        )
        .await
    }

    /// `list_current_states` with each decision's indexed output, in one read.
    pub async fn list_current_states_with_outputs(
        &self,
        character_id: &str,
        query: CurrentStateQuery,
    ) -> Result<DecisionOutputPage> {
        let filter = current_filter(query.state)?;
        self.decisions_with_outputs(
            character_id,
            None,
            query.spec_version,
            Some(filter),
            query.limit,
            query.cursor,
        )
        .await
    }

    /// `list_approved_outputs` with each decision's indexed output, in one read.
    pub async fn list_approved_with_outputs(
        &self,
        character_id: &str,
        query: CurrentStateQuery,
    ) -> Result<DecisionOutputPage> {
        self.list_current_states_with_outputs(
            character_id,
            CurrentStateQuery {
                state: Some(Decision::Approved),
                ..query
            },
        )
        .await
    }

    async fn decisions_with_outputs(
        &self,
        character_id: &str,
        output_id: Option<String>,
        spec_version: Option<u32>,
        current: Option<CurrentFilter>,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<DecisionOutputPage> {
        let (rows, next_cursor) = self
            .repository()
            .list_decisions_with_outputs(
                character_id.to_owned(),
                output_id,
                spec_version,
                current,
                limit,
                cursor,
            )
            .await?;
        Ok(DecisionOutputPage {
            decisions: rows
                .into_iter()
                .map(|(decision, output)| DecisionWithOutput { decision, output })
                .collect(),
            next_cursor,
        })
    }

    /// Runs submitted against the character, newest first.
    pub async fn list_runs(&self, character_id: &str, query: CharacterRunQuery) -> Result<RunPage> {
        self.repository()
            .list_character_runs(
                character_id.to_owned(),
                query.spec_version,
                query.status,
                query.limit,
                query.cursor,
            )
            .await
    }

    /// Outputs of completed runs submitted against the character, newest first.
    pub async fn list_outputs(
        &self,
        character_id: &str,
        query: CharacterRunQuery,
    ) -> Result<OutputPage> {
        self.repository()
            .list_character_outputs(
                character_id.to_owned(),
                query.spec_version,
                query.limit,
                query.cursor,
            )
            .await
    }
}

fn current_filter(state: Option<Decision>) -> Result<CurrentFilter> {
    match state {
        None => Ok(CurrentFilter::Any),
        Some(Decision::Cleared) => Err(HarnessError::InvalidCharacterRequest(
            "A cleared output has no current state; filter by approved or rejected.".into(),
        )),
        Some(state) => Ok(CurrentFilter::Only(state)),
    }
}
