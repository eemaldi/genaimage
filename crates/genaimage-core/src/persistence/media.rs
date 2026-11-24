use chrono::DateTime;

use super::{map_output, params, AsyncRunRepository, OptionalExtension, RunRepository};
use crate::{
    media::{IndexedOutput, OutputListQuery, OutputPage},
    HarnessError, Result,
};

const OUTPUT_SELECT: &str = "
    SELECT o.run_id, o.node_id, o.kind, o.source_filename, o.source_subfolder,
           o.source_type, o.local_path, o.sha256, o.byte_size, o.created_at,
           o.id, r.workflow_id, COALESCE(m.tags_json, '[]')
    FROM outputs o JOIN runs r ON r.id = o.run_id
    LEFT JOIN run_media m ON m.run_id = r.id
    ";

fn map_indexed(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedOutput> {
    Ok(IndexedOutput {
        id: format!("{}:{}", row.get::<_, String>(0)?, row.get::<_, i64>(10)?),
        output: map_output(row)?,
        workflow_id: row.get(11)?,
        tags: serde_json::from_str(&row.get::<_, String>(12)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

// Keep the raw timestamp as the sort key, as for runs; the row ID resolves ties.
#[derive(serde::Deserialize, serde::Serialize)]
struct Cursor(String, i64);

impl Cursor {
    fn decode(value: &str) -> Result<Self> {
        let invalid = || HarnessError::InvalidMediaRequest("Invalid output cursor.");
        let raw = value
            .strip_prefix("v1.")
            .filter(|raw| raw.len() <= 1024)
            .ok_or_else(invalid)?;
        let cursor: Self = serde_json::from_slice(&hex::decode(raw).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
        DateTime::parse_from_rfc3339(&cursor.0).map_err(|_| invalid())?;
        if cursor.1 <= 0 {
            return Err(invalid());
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String> {
        Ok(format!("v1.{}", hex::encode(serde_json::to_vec(self)?)))
    }
}

impl RunRepository {
    fn list_outputs(&self, query: OutputListQuery) -> Result<OutputPage> {
        let limit = query.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            return Err(HarnessError::InvalidMediaRequest(
                "Limit must be between 1 and 200.",
            ));
        }
        if matches!((query.created_from, query.created_to), (Some(from), Some(to)) if from > to) {
            return Err(HarnessError::InvalidMediaRequest(
                "created_from must not be later than created_to.",
            ));
        }
        let cursor = query.cursor.as_deref().map(Cursor::decode).transpose()?;
        self.guarded_read(|connection| {
            let mut sql = OUTPUT_SELECT.to_owned();
            let status = query.status.unwrap_or(crate::RunStatus::Completed);
            if status == crate::RunStatus::Completed {
                sql.push_str(" WHERE r.status = 'completed' AND r.status = ?9");
            } else if status.is_terminal() {
                sql.push_str(" WHERE r.status IN ('failed', 'cancelled') AND r.status = ?9");
            } else {
                return Ok(OutputPage {
                    outputs: Vec::new(),
                    next_cursor: None,
                });
            }
            sql.push_str(if cursor.is_some() {
                " AND (o.created_at, o.id) < (?1, ?2)"
            } else {
                " AND ?1 IS NULL AND ?2 IS NULL"
            });
            // Keep active range/equality predicates usable by SQLite's indexes.
            for (present, condition, absent) in [
                (query.run.is_some(), " AND o.run_id = ?3", " AND ?3 IS NULL"),
                (
                    query.workflow.is_some(),
                    " AND r.workflow_id = ?4",
                    " AND ?4 IS NULL",
                ),
                (
                    query.created_from.is_some(),
                    " AND o.created_at >= ?5",
                    " AND ?5 IS NULL",
                ),
                (
                    query.created_to.is_some(),
                    " AND o.created_at <= ?6",
                    " AND ?6 IS NULL",
                ),
                (
                    query.tag.is_some(),
                    " AND EXISTS (SELECT 1 FROM json_each(m.tags_json) WHERE value = ?7)",
                    " AND ?7 IS NULL",
                ),
            ] {
                sql.push_str(if present { condition } else { absent });
            }
            sql.push_str(" ORDER BY o.created_at DESC, o.id DESC LIMIT ?8");
            let mut stmt = connection.prepare(&sql)?;
            let mut rows = stmt
                .query_map(
                    params![
                        cursor.as_ref().map(|cursor| &cursor.0),
                        cursor.as_ref().map(|cursor| cursor.1),
                        query.run,
                        query.workflow,
                        query.created_from.map(|time| time.to_rfc3339()),
                        query.created_to.map(|time| time.to_rfc3339()),
                        query.tag,
                        (limit + 1) as i64,
                        status.as_str(),
                    ],
                    |row| Ok((map_indexed(row)?, Cursor(row.get(9)?, row.get(10)?))),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let next_cursor = if rows.len() > limit {
                rows.pop();
                rows.last().map(|(_, cursor)| cursor.encode()).transpose()?
            } else {
                None
            };
            Ok(OutputPage {
                outputs: rows.into_iter().map(|(output, _)| output).collect(),
                next_cursor,
            })
        })
    }
}

impl AsyncRunRepository {
    pub async fn list_outputs(&self, query: OutputListQuery) -> Result<OutputPage> {
        self.call(move |repo| repo.list_outputs(query)).await
    }

    pub async fn output(&self, id: &str) -> Result<IndexedOutput> {
        let (run, row) = id.rsplit_once(':').ok_or(HarnessError::OutputNotFound)?;
        let row: i64 = row.parse().map_err(|_| HarnessError::OutputNotFound)?;
        let run = run.to_owned();
        self.call(move |repo| {
            repo.guarded_read(|connection| {
                connection
                    .query_row(
                        &format!("{OUTPUT_SELECT} WHERE o.id = ?1 AND o.run_id = ?2 AND r.status IN ('completed', 'failed', 'cancelled')"),
                        params![row, run],
                        map_indexed,
                    )
                    .optional()?
                    .ok_or(HarnessError::OutputNotFound)
            })
        })
        .await
    }

    pub(crate) async fn record_run_media(
        &self,
        id: String,
        sha256: String,
        tags: Vec<String>,
    ) -> Result<()> {
        let tags = serde_json::to_string(&tags)?;
        self.call(move |repo| repo.guarded(|connection| {
            connection.execute("INSERT INTO run_media (run_id, manifest_sha256, tags_json) VALUES (?1, ?2, ?3) ON CONFLICT(run_id) DO NOTHING", params![id, sha256, tags])?;
            let recorded: String = connection.query_row("SELECT manifest_sha256 FROM run_media WHERE run_id = ?1", [&id], |row| row.get(0))?;
            if sha256 != recorded {
                return Err(HarnessError::MediaIntegrity);
            }
            Ok(())
        })).await
    }

    pub(crate) async fn manifest_checksum(&self, id: String) -> Result<Option<String>> {
        self.call(move |repo| {
            repo.guarded_read(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT manifest_sha256 FROM run_media WHERE run_id = ?1",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
        })
        .await
    }
}
