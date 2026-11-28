use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

use super::{
    map_run,
    media::{map_indexed, OUTPUT_SELECT},
    parse_sqlite_time, AsyncRunRepository, RunCursor, RunPage, RunRepository, RunStatus,
};
use crate::{
    characters::{
        Character, CharacterLink, CharacterReference, CharacterSpec, CharacterSpecVersion,
        Decision, OutputDecision, ReferenceRole, DEFAULT_PAGE_LIMIT, MAX_ACTIVE_REFERENCES,
        MAX_PAGE_LIMIT,
    },
    media::OutputPage,
    HarnessError, Result,
};

fn invalid(message: &str) -> HarnessError {
    HarnessError::InvalidCharacterRequest(message.to_owned())
}

pub(crate) fn page_limit(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(HarnessError::InvalidCharacterRequest(format!(
            "Limit must be between 1 and {MAX_PAGE_LIMIT}."
        )));
    }
    Ok(limit)
}

/// An opaque position in a list ordered by an increasing integer key.
pub(crate) fn decode_cursor(cursor: Option<&str>) -> Result<Option<i64>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    cursor
        .strip_prefix("v1.")
        .filter(|raw| raw.len() <= 32)
        .and_then(|raw| hex::decode(raw).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.parse::<i64>().ok())
        .filter(|position| *position > 0)
        .map(Some)
        .ok_or_else(|| invalid("Invalid cursor."))
}

fn encode_cursor(position: i64) -> String {
    format!("v1.{}", hex::encode(position.to_string()))
}

/// Rows carry their position; the extra row fetched past the limit proves that a
/// next page exists.
fn into_page<T>(mut rows: Vec<(i64, T)>, limit: usize) -> (Vec<T>, Option<String>) {
    let next = if rows.len() > limit {
        rows.pop();
        rows.last().map(|(position, _)| encode_cursor(*position))
    } else {
        None
    };
    (rows.into_iter().map(|(_, row)| row).collect(), next)
}

fn corrupt(index: usize, error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn stored_u32(row: &Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value: i64 = row.get(index)?;
    u32::try_from(value).map_err(|error| corrupt(index, error))
}

fn stored_json<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| corrupt(index, error))
}

fn stored_enum<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(value)).map_err(|error| corrupt(index, error))
}

fn now_text() -> (DateTime<Utc>, String) {
    let now = Utc::now();
    (now, now.to_rfc3339())
}

const CHARACTER_SELECT: &str = "SELECT c.seq, c.id, c.name, c.created_at,
    (SELECT MAX(version) FROM character_spec_versions v WHERE v.character_id = c.id)
    FROM characters c";

fn map_character(row: &Row<'_>) -> rusqlite::Result<(i64, Character)> {
    Ok((
        row.get(0)?,
        Character {
            id: row.get(1)?,
            name: row.get(2)?,
            created_at: parse_sqlite_time(row, 3)?,
            latest_spec_version: row
                .get::<_, Option<i64>>(4)?
                .map(|version| u32::try_from(version).map_err(|error| corrupt(4, error)))
                .transpose()?,
        },
    ))
}

fn load_character(connection: &Connection, id: &str) -> Result<Character> {
    connection
        .query_row(
            &format!("{CHARACTER_SELECT} WHERE c.id = ?1"),
            [id],
            map_character,
        )
        .optional()?
        .map(|(_, character)| character)
        .ok_or_else(|| HarnessError::CharacterNotFound {
            character_id: id.to_owned(),
        })
}

fn require_character(connection: &Connection, id: &str) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM characters WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(HarnessError::CharacterNotFound {
            character_id: id.to_owned(),
        })
    }
}

fn latest_version(connection: &Connection, character_id: &str) -> Result<Option<u32>> {
    let latest: Option<i64> = connection.query_row(
        "SELECT MAX(version) FROM character_spec_versions WHERE character_id = ?1",
        [character_id],
        |row| row.get(0),
    )?;
    latest
        .map(|version| {
            u32::try_from(version).map_err(|_| {
                HarnessError::Database(format!(
                    "character '{character_id}' has the invalid stored spec version {version}"
                ))
            })
        })
        .transpose()
}

const VERSION_SELECT: &str =
    "SELECT character_id, version, based_on, spec_json, note, created_at FROM character_spec_versions";

fn map_version(row: &Row<'_>) -> rusqlite::Result<CharacterSpecVersion> {
    Ok(CharacterSpecVersion {
        character_id: row.get(0)?,
        version: stored_u32(row, 1)?,
        based_on: row
            .get::<_, Option<i64>>(2)?
            .map(|version| u32::try_from(version).map_err(|error| corrupt(2, error)))
            .transpose()?,
        spec: stored_json(row, 3)?,
        note: row.get(4)?,
        created_at: parse_sqlite_time(row, 5)?,
    })
}

fn load_version(
    connection: &Connection,
    character_id: &str,
    version: u32,
) -> Result<CharacterSpecVersion> {
    connection
        .query_row(
            &format!("{VERSION_SELECT} WHERE character_id = ?1 AND version = ?2"),
            params![character_id, version],
            map_version,
        )
        .optional()?
        .ok_or_else(|| missing_version(character_id, version))
}

fn missing_version(character_id: &str, version: u32) -> HarnessError {
    HarnessError::CharacterSpecVersionNotFound {
        character_id: character_id.to_owned(),
        version,
    }
}

/// The check and the insert share the caller's write transaction, so two creators
/// cannot both succeed against the same latest version.
fn insert_version(
    connection: &Connection,
    character_id: &str,
    based_on: Option<u32>,
    spec: &CharacterSpec,
    note: Option<&str>,
    created_at: DateTime<Utc>,
) -> Result<CharacterSpecVersion> {
    let latest = latest_version(connection, character_id)?;
    if latest != based_on {
        return Err(HarnessError::CharacterSpecConflict {
            character_id: character_id.to_owned(),
            based_on,
            latest,
        });
    }
    let version = latest.unwrap_or(0).checked_add(1).ok_or_else(|| {
        HarnessError::InvalidCharacterRequest(
            "The character has reached its last spec version.".into(),
        )
    })?;
    connection.execute(
        "INSERT INTO character_spec_versions
            (character_id, version, based_on, spec_json, note, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            character_id,
            version,
            based_on,
            serde_json::to_string(spec)?,
            note,
            created_at.to_rfc3339()
        ],
    )?;
    Ok(CharacterSpecVersion {
        character_id: character_id.to_owned(),
        version,
        created_at,
        based_on,
        spec: spec.clone(),
        note: note.map(str::to_owned),
    })
}

const REFERENCE_COLUMNS: &str =
    "SELECT id, character_id, upload_id, role, note, added_at, removed_at";

fn map_reference(row: &Row<'_>) -> rusqlite::Result<CharacterReference> {
    Ok(CharacterReference {
        id: row.get(0)?,
        character_id: row.get(1)?,
        upload_id: row.get(2)?,
        role: stored_enum(row, 3)?,
        note: row.get(4)?,
        added_at: parse_sqlite_time(row, 5)?,
        removed_at: row
            .get::<_, Option<String>>(6)?
            .map(|_| parse_sqlite_time(row, 6))
            .transpose()?,
    })
}

fn load_reference(
    connection: &Connection,
    character_id: &str,
    reference_id: &str,
) -> Result<CharacterReference> {
    connection
        .query_row(
            &format!(
                "{REFERENCE_COLUMNS} FROM character_references WHERE id = ?1 AND character_id = ?2"
            ),
            params![reference_id, character_id],
            map_reference,
        )
        .optional()?
        .ok_or_else(|| HarnessError::CharacterReferenceNotFound {
            character_id: character_id.to_owned(),
            reference_id: reference_id.to_owned(),
        })
}

const DECISION_SELECT: &str = "SELECT d.seq, d.id, d.character_id, d.output_id, d.spec_version,
    d.decision, d.note, d.decided_at FROM character_output_decisions d";

fn map_decision(row: &Row<'_>) -> rusqlite::Result<(i64, OutputDecision)> {
    Ok((
        row.get(0)?,
        OutputDecision {
            id: row.get(1)?,
            character_id: row.get(2)?,
            output_id: row.get(3)?,
            spec_version: stored_u32(row, 4)?,
            decision: stored_enum(row, 5)?,
            note: row.get(6)?,
            decided_at: parse_sqlite_time(row, 7)?,
        },
    ))
}

fn current_decision(
    connection: &Connection,
    character_id: &str,
    output_id: &str,
) -> Result<Option<OutputDecision>> {
    let latest = connection
        .query_row(
            &format!(
                "{DECISION_SELECT} WHERE d.character_id = ?1 AND d.output_id = ?2
                 ORDER BY d.seq DESC LIMIT 1"
            ),
            params![character_id, output_id],
            map_decision,
        )
        .optional()?
        .map(|(_, decision)| decision);
    Ok(latest.filter(|decision| decision.decision != Decision::Cleared))
}

/// Outputs are addressed as `{run_id}:{row}`. They are never deleted, so the
/// existence check cannot go stale; it runs in the write transaction anyway.
fn output_run_status(connection: &Connection, output_id: &str) -> Result<RunStatus> {
    let (run, row) = output_id
        .rsplit_once(':')
        .ok_or(HarnessError::OutputNotFound)?;
    let row: i64 = row.parse().map_err(|_| HarnessError::OutputNotFound)?;
    let status: String = connection
        .query_row(
            "SELECT r.status FROM outputs o JOIN runs r ON r.id = o.run_id
             WHERE o.id = ?1 AND o.run_id = ?2",
            params![row, run],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(HarnessError::OutputNotFound)?;
    RunStatus::parse(&status)
}

pub(crate) fn verify_link(connection: &Connection, link: &CharacterLink) -> Result<()> {
    require_character(connection, &link.id)?;
    load_version(connection, &link.id, link.spec_version).map(|_| ())
}

pub(crate) fn link_run(connection: &Connection, run_id: &str, link: &CharacterLink) -> Result<()> {
    verify_link(connection, link)?;
    connection.execute(
        "UPDATE runs SET linked_character_id = ?2, linked_character_spec_version = ?3 WHERE id = ?1",
        params![run_id, link.id, link.spec_version],
    )?;
    Ok(())
}

pub(crate) struct DecisionQuery<'a> {
    pub character_id: &'a str,
    pub output_id: Option<&'a str>,
    pub spec_version: Option<u32>,
    pub current: Option<CurrentFilter>,
    pub limit: Option<usize>,
    pub cursor: Option<&'a str>,
}

/// Which current states a query keeps: every uncleared one, or just one kind.
#[derive(Clone, Copy)]
pub(crate) enum CurrentFilter {
    Any,
    Only(Decision),
}

impl RunRepository {
    pub(crate) fn create_character(
        &self,
        name: &str,
        first: Option<(&CharacterSpec, Option<&str>)>,
    ) -> Result<(Character, Option<CharacterSpecVersion>)> {
        let id = format!("char-{}", Uuid::new_v4());
        self.guarded(|connection| {
            let (created_at, created_text) = now_text();
            connection.execute(
                "INSERT INTO characters (id, name, created_at) VALUES (?1, ?2, ?3)",
                params![id, name, created_text],
            )?;
            let version = first
                .map(|(spec, note)| insert_version(connection, &id, None, spec, note, created_at))
                .transpose()?;
            Ok((
                Character {
                    id: id.clone(),
                    name: name.to_owned(),
                    created_at,
                    latest_spec_version: version.as_ref().map(|version| version.version),
                },
                version,
            ))
        })
    }

    pub(crate) fn get_character(&self, id: &str) -> Result<Character> {
        self.guarded_read(|connection| load_character(connection, id))
    }

    pub(crate) fn list_characters(
        &self,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<(Vec<Character>, Option<String>)> {
        let limit = page_limit(limit)?;
        let cursor = decode_cursor(cursor)?;
        self.guarded_read(|connection| {
            let rows = connection
                .prepare(&format!(
                    "{CHARACTER_SELECT} WHERE (?1 IS NULL OR c.seq < ?1) ORDER BY c.seq DESC LIMIT ?2"
                ))?
                .query_map(params![cursor, (limit + 1) as i64], map_character)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(into_page(rows, limit))
        })
    }

    pub(crate) fn create_spec_version(
        &self,
        character_id: &str,
        based_on: Option<u32>,
        spec: &CharacterSpec,
        note: Option<&str>,
    ) -> Result<CharacterSpecVersion> {
        self.guarded(|connection| {
            require_character(connection, character_id)?;
            insert_version(connection, character_id, based_on, spec, note, Utc::now())
        })
    }

    pub(crate) fn get_spec_version(
        &self,
        character_id: &str,
        version: u32,
    ) -> Result<CharacterSpecVersion> {
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            load_version(connection, character_id, version)
        })
    }

    pub(crate) fn latest_spec_version(
        &self,
        character_id: &str,
    ) -> Result<Option<CharacterSpecVersion>> {
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            latest_version(connection, character_id)?
                .map(|version| load_version(connection, character_id, version))
                .transpose()
        })
    }

    pub(crate) fn list_spec_versions(
        &self,
        character_id: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<(Vec<CharacterSpecVersion>, Option<String>)> {
        let limit = page_limit(limit)?;
        let cursor = decode_cursor(cursor)?;
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            let rows = connection
                .prepare(&format!(
                    "{VERSION_SELECT} WHERE character_id = ?1 AND (?2 IS NULL OR version < ?2)
                     ORDER BY version DESC LIMIT ?3"
                ))?
                .query_map(params![character_id, cursor, (limit + 1) as i64], |row| {
                    Ok((row.get::<_, i64>(1)?, map_version(row)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(into_page(rows, limit))
        })
    }

    pub(crate) fn add_reference(
        &self,
        character_id: &str,
        upload_id: &str,
        role: ReferenceRole,
        note: Option<&str>,
    ) -> Result<CharacterReference> {
        let id = format!("charref-{}", Uuid::new_v4());
        self.guarded(|connection| {
            require_character(connection, character_id)?;
            let existing: Option<String> = connection
                .query_row(
                    "SELECT id FROM character_references
                     WHERE character_id = ?1 AND upload_id = ?2 AND role = ?3 AND removed_at IS NULL",
                    params![character_id, upload_id, role.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                return Err(HarnessError::CharacterConflict(format!(
                    "Upload '{upload_id}' is already attached to character '{character_id}' as a {} (reference '{existing}'); nothing was written.",
                    role.as_str()
                )));
            }
            let active: i64 = connection.query_row(
                "SELECT COUNT(*) FROM character_references
                 WHERE character_id = ?1 AND removed_at IS NULL",
                [character_id],
                |row| row.get(0),
            )?;
            if active >= MAX_ACTIVE_REFERENCES as i64 {
                return Err(HarnessError::InvalidCharacterRequest(format!(
                    "A character may hold at most {MAX_ACTIVE_REFERENCES} active references; remove one first. Nothing was written."
                )));
            }
            let (added_at, added_text) = now_text();
            connection.execute(
                "INSERT INTO character_references (id, character_id, upload_id, role, note, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, character_id, upload_id, role.as_str(), note, added_text],
            )?;
            Ok(CharacterReference {
                id: id.clone(),
                character_id: character_id.to_owned(),
                upload_id: upload_id.to_owned(),
                role,
                note: note.map(str::to_owned),
                added_at,
                removed_at: None,
            })
        })
    }

    /// Removing an already removed reference changes nothing and returns it as recorded.
    pub(crate) fn remove_reference(
        &self,
        character_id: &str,
        reference_id: &str,
    ) -> Result<CharacterReference> {
        self.guarded(|connection| {
            require_character(connection, character_id)?;
            let reference = load_reference(connection, character_id, reference_id)?;
            if reference.removed_at.is_some() {
                return Ok(reference);
            }
            connection.execute(
                "UPDATE character_references SET removed_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
                params![reference_id, Utc::now().to_rfc3339()],
            )?;
            load_reference(connection, character_id, reference_id)
        })
    }

    pub(crate) fn list_references(
        &self,
        character_id: &str,
        include_removed: bool,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<(Vec<CharacterReference>, Option<String>)> {
        let limit = page_limit(limit)?;
        let cursor = decode_cursor(cursor)?;
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            let rows = connection
                .prepare(&format!(
                    "{REFERENCE_COLUMNS}, seq FROM character_references
                     WHERE character_id = ?1 AND (?2 OR removed_at IS NULL)
                       AND (?3 IS NULL OR seq < ?3)
                     ORDER BY seq DESC LIMIT ?4"
                ))?
                .query_map(
                    params![character_id, include_removed, cursor, (limit + 1) as i64],
                    |row| Ok((row.get::<_, i64>(7)?, map_reference(row)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(into_page(rows, limit))
        })
    }

    pub(crate) fn record_decision(
        &self,
        character_id: &str,
        output_id: &str,
        spec_version: u32,
        decision: Decision,
        note: Option<&str>,
    ) -> Result<OutputDecision> {
        let id = format!("chardec-{}", Uuid::new_v4());
        self.guarded(|connection| {
            require_character(connection, character_id)?;
            load_version(connection, character_id, spec_version)?;
            let run_status = output_run_status(connection, output_id)?;
            match decision {
                Decision::Cleared => {
                    if current_decision(connection, character_id, output_id)?.is_none() {
                        return Err(HarnessError::CharacterConflict(format!(
                            "Output '{output_id}' has no current decision for character '{character_id}' to clear; nothing was written."
                        )));
                    }
                }
                Decision::Approved | Decision::Rejected => {
                    if run_status != RunStatus::Completed {
                        return Err(HarnessError::InvalidCharacterRequest(format!(
                            "Output '{output_id}' belongs to a run that is {}; only outputs of completed runs can be approved or rejected. Nothing was written.",
                            run_status.as_str()
                        )));
                    }
                }
            }
            let (decided_at, decided_text) = now_text();
            connection.execute(
                "INSERT INTO character_output_decisions
                    (id, character_id, output_id, spec_version, decision, note, decided_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    character_id,
                    output_id,
                    spec_version,
                    decision.as_str(),
                    note,
                    decided_text
                ],
            )?;
            Ok(OutputDecision {
                id: id.clone(),
                character_id: character_id.to_owned(),
                output_id: output_id.to_owned(),
                spec_version,
                decision,
                note: note.map(str::to_owned),
                decided_at,
            })
        })
    }

    pub(crate) fn current_decision(
        &self,
        character_id: &str,
        output_id: &str,
    ) -> Result<Option<OutputDecision>> {
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            current_decision(connection, character_id, output_id)
        })
    }

    /// History when `current` is `None`, the derived current states otherwise.
    pub(crate) fn list_decisions(
        &self,
        query: DecisionQuery<'_>,
    ) -> Result<(Vec<OutputDecision>, Option<String>)> {
        let limit = page_limit(query.limit)?;
        let cursor = decode_cursor(query.cursor)?;
        let (only_current, kind) = match query.current {
            None => (false, None),
            Some(CurrentFilter::Any) => (true, None),
            Some(CurrentFilter::Only(kind)) => (true, Some(kind.as_str())),
        };
        self.guarded_read(|connection| {
            require_character(connection, query.character_id)?;
            let rows = connection
                .prepare(&format!(
                    "{DECISION_SELECT}
                     WHERE d.character_id = ?1
                       AND (?2 IS NULL OR d.output_id = ?2)
                       AND (?3 IS NULL OR d.spec_version = ?3)
                       AND (?4 IS NULL OR d.seq < ?4)
                       AND (?5 = 0 OR (d.decision <> 'cleared'
                            AND d.seq = (SELECT MAX(l.seq) FROM character_output_decisions l
                                         WHERE l.character_id = d.character_id
                                           AND l.output_id = d.output_id)))
                       AND (?6 IS NULL OR d.decision = ?6)
                     ORDER BY d.seq DESC LIMIT ?7"
                ))?
                .query_map(
                    params![
                        query.character_id,
                        query.output_id,
                        query.spec_version,
                        cursor,
                        only_current,
                        kind,
                        (limit + 1) as i64
                    ],
                    map_decision,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(into_page(rows, limit))
        })
    }

    pub(crate) fn verify_character_link(&self, link: &CharacterLink) -> Result<()> {
        self.guarded_read(|connection| verify_link(connection, link))
    }

    pub(crate) fn list_character_runs(
        &self,
        link_id: &str,
        spec_version: Option<u32>,
        status: Option<RunStatus>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<RunPage> {
        let limit = page_limit(limit)?;
        let cursor = cursor
            .map(RunCursor::decode)
            .transpose()
            .map_err(|_| invalid("Invalid cursor."))?;
        self.guarded_read(|connection| {
            require_character(connection, link_id)?;
            let mut rows = connection
                .prepare(
                    "SELECT * FROM runs
                     WHERE linked_character_id = ?1
                       AND (?2 IS NULL OR linked_character_spec_version = ?2)
                       AND (?3 IS NULL OR status = ?3)
                       AND (?4 IS NULL OR (created_at, id) < (?4, ?5))
                     ORDER BY created_at DESC, id DESC LIMIT ?6",
                )?
                .query_map(
                    params![
                        link_id,
                        spec_version,
                        status.map(RunStatus::as_str),
                        cursor.as_ref().map(|cursor| &cursor.0),
                        cursor.as_ref().map(|cursor| &cursor.1),
                        (limit + 1) as i64
                    ],
                    |row| Ok((map_run(row)?, row.get::<_, String>("created_at")?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if rows.len() > limit {
                rows.pop();
                rows.last()
                    .map(|(run, created_at)| RunCursor(created_at.clone(), run.id.clone()).encode())
                    .transpose()?
            } else {
                None
            };
            Ok(RunPage {
                runs: rows.into_iter().map(|(run, _)| run).collect(),
                next_cursor,
            })
        })
    }

    /// Outputs of completed runs submitted against the character.
    pub(crate) fn list_character_outputs(
        &self,
        character_id: &str,
        spec_version: Option<u32>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<OutputPage> {
        let limit = page_limit(limit)?;
        let cursor = decode_cursor(cursor)?;
        self.guarded_read(|connection| {
            require_character(connection, character_id)?;
            let mut rows = connection
                .prepare(&format!(
                    "{OUTPUT_SELECT}
                     WHERE r.linked_character_id = ?1 AND r.status = 'completed'
                       AND (?2 IS NULL OR r.linked_character_spec_version = ?2)
                       AND (?3 IS NULL OR o.id < ?3)
                     ORDER BY o.id DESC LIMIT ?4"
                ))?
                .query_map(
                    params![character_id, spec_version, cursor, (limit + 1) as i64],
                    |row| Ok((row.get::<_, i64>(10)?, map_indexed(row)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if rows.len() > limit {
                rows.pop();
                rows.last().map(|(position, _)| encode_cursor(*position))
            } else {
                None
            };
            Ok(OutputPage {
                outputs: rows.into_iter().map(|(_, output)| output).collect(),
                next_cursor,
            })
        })
    }
}

impl AsyncRunRepository {
    pub(crate) async fn create_character(
        &self,
        name: String,
        first: Option<(CharacterSpec, Option<String>)>,
    ) -> Result<(Character, Option<CharacterSpecVersion>)> {
        self.call(move |repo| {
            repo.create_character(
                &name,
                first.as_ref().map(|(spec, note)| (spec, note.as_deref())),
            )
        })
        .await
    }

    pub(crate) async fn get_character(&self, id: String) -> Result<Character> {
        self.call(move |repo| repo.get_character(&id)).await
    }

    pub(crate) async fn list_characters(
        &self,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<(Vec<Character>, Option<String>)> {
        self.call(move |repo| repo.list_characters(limit, cursor.as_deref()))
            .await
    }

    pub(crate) async fn create_spec_version(
        &self,
        character_id: String,
        based_on: Option<u32>,
        spec: CharacterSpec,
        note: Option<String>,
    ) -> Result<CharacterSpecVersion> {
        self.call(move |repo| {
            repo.create_spec_version(&character_id, based_on, &spec, note.as_deref())
        })
        .await
    }

    pub(crate) async fn get_spec_version(
        &self,
        character_id: String,
        version: u32,
    ) -> Result<CharacterSpecVersion> {
        self.call(move |repo| repo.get_spec_version(&character_id, version))
            .await
    }

    pub(crate) async fn latest_spec_version(
        &self,
        character_id: String,
    ) -> Result<Option<CharacterSpecVersion>> {
        self.call(move |repo| repo.latest_spec_version(&character_id))
            .await
    }

    pub(crate) async fn list_spec_versions(
        &self,
        character_id: String,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<(Vec<CharacterSpecVersion>, Option<String>)> {
        self.call(move |repo| repo.list_spec_versions(&character_id, limit, cursor.as_deref()))
            .await
    }

    pub(crate) async fn add_reference(
        &self,
        character_id: String,
        upload_id: String,
        role: ReferenceRole,
        note: Option<String>,
    ) -> Result<CharacterReference> {
        self.call(move |repo| repo.add_reference(&character_id, &upload_id, role, note.as_deref()))
            .await
    }

    pub(crate) async fn remove_reference(
        &self,
        character_id: String,
        reference_id: String,
    ) -> Result<CharacterReference> {
        self.call(move |repo| repo.remove_reference(&character_id, &reference_id))
            .await
    }

    pub(crate) async fn list_references(
        &self,
        character_id: String,
        include_removed: bool,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<(Vec<CharacterReference>, Option<String>)> {
        self.call(move |repo| {
            repo.list_references(&character_id, include_removed, limit, cursor.as_deref())
        })
        .await
    }

    pub(crate) async fn record_decision(
        &self,
        character_id: String,
        output_id: String,
        spec_version: u32,
        decision: Decision,
        note: Option<String>,
    ) -> Result<OutputDecision> {
        self.call(move |repo| {
            repo.record_decision(
                &character_id,
                &output_id,
                spec_version,
                decision,
                note.as_deref(),
            )
        })
        .await
    }

    pub(crate) async fn current_decision(
        &self,
        character_id: String,
        output_id: String,
    ) -> Result<Option<OutputDecision>> {
        self.call(move |repo| repo.current_decision(&character_id, &output_id))
            .await
    }

    pub(crate) async fn list_decisions(
        &self,
        character_id: String,
        output_id: Option<String>,
        spec_version: Option<u32>,
        current: Option<CurrentFilter>,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<(Vec<OutputDecision>, Option<String>)> {
        self.call(move |repo| {
            repo.list_decisions(DecisionQuery {
                character_id: &character_id,
                output_id: output_id.as_deref(),
                spec_version,
                current,
                limit,
                cursor: cursor.as_deref(),
            })
        })
        .await
    }

    pub(crate) async fn verify_character_link(&self, link: CharacterLink) -> Result<()> {
        self.call(move |repo| repo.verify_character_link(&link))
            .await
    }

    pub(crate) async fn list_character_runs(
        &self,
        character_id: String,
        spec_version: Option<u32>,
        status: Option<RunStatus>,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<RunPage> {
        self.call(move |repo| {
            repo.list_character_runs(
                &character_id,
                spec_version,
                status,
                limit,
                cursor.as_deref(),
            )
        })
        .await
    }

    pub(crate) async fn list_character_outputs(
        &self,
        character_id: String,
        spec_version: Option<u32>,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<OutputPage> {
        self.call(move |repo| {
            repo.list_character_outputs(&character_id, spec_version, limit, cursor.as_deref())
        })
        .await
    }
}
