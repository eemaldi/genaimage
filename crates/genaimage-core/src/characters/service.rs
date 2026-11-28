use serde::{Deserialize, Serialize};

use super::{
    validate_name, validate_note, validate_output_id, Character, CharacterReference, CharacterSpec,
    CharacterSpecVersion, Decision, OutputDecision, ReferenceRole,
};
use crate::{
    media::OutputPage,
    persistence::{characters::CurrentFilter, RunPage},
    Harness, HarnessError, Result, RunStatus,
};

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

/// Characters, their versioned specs, references and output decisions.
///
/// Every write is one transaction: when a method returns an error, nothing was
/// written, and the message says so where that is not obvious.
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
    pub async fn create_character(&self, request: NewCharacter) -> Result<CreatedCharacter> {
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
        let first = request.spec.map(|spec| (spec, request.note));
        let (character, spec_version) = self
            .repository()
            .create_character(request.name, first)
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
    ) -> Result<CharacterSpecVersion> {
        request.spec.validate()?;
        validate_note("note", request.note.as_deref())?;
        self.repository()
            .create_spec_version(
                character_id.to_owned(),
                request.based_on,
                request.spec,
                request.note,
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
    ) -> Result<CharacterReference> {
        validate_note("note", request.note.as_deref())?;
        if request.upload_id.len() > UPLOAD_ID_MAX_BYTES {
            return Err(HarnessError::InvalidCharacterRequest(format!(
                "upload_id must be at most {UPLOAD_ID_MAX_BYTES} bytes."
            )));
        }
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
        self.repository()
            .add_reference(
                character_id.to_owned(),
                request.upload_id,
                request.role,
                request.note,
            )
            .await
    }

    /// A soft removal: the row stays, with `removed_at` set once. Removing a
    /// reference that is already removed returns it unchanged.
    pub async fn remove_reference(
        &self,
        character_id: &str,
        reference_id: &str,
    ) -> Result<CharacterReference> {
        self.repository()
            .remove_reference(character_id.to_owned(), reference_id.to_owned())
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
    /// rejected, belong to a completed run; the spec version must exist. A
    /// `cleared` decision needs a current decision to undo.
    pub async fn record_decision(
        &self,
        character_id: &str,
        request: NewDecision,
    ) -> Result<OutputDecision> {
        validate_output_id(&request.output_id)?;
        validate_note("note", request.note.as_deref())?;
        self.repository()
            .record_decision(
                character_id.to_owned(),
                request.output_id,
                request.spec_version,
                request.decision,
                request.note,
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
        let filter = match query.state {
            None => CurrentFilter::Any,
            Some(Decision::Cleared) => {
                return Err(HarnessError::InvalidCharacterRequest(
                    "A cleared output has no current state; filter by approved or rejected.".into(),
                ))
            }
            Some(state) => CurrentFilter::Only(state),
        };
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
