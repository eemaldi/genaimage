use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use super::{
    map_run, page_limit, parse_json_column, parse_sqlite_time, AsyncRunRepository, RunCursor,
    RunRecord, RunRepository, RunStatus,
};
use crate::{HarnessError, Result};

/// Lifecycle of the submission driver, not of the runs it creates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchState {
    Submitting,
    Interrupted,
    Finished,
}

impl BatchState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Submitting => "submitting",
            Self::Interrupted => "interrupted",
            Self::Finished => "finished",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "submitting" => Ok(Self::Submitting),
            "interrupted" => Ok(Self::Interrupted),
            "finished" => Ok(Self::Finished),
            _ => Err(HarnessError::Database(format!(
                "unknown persisted batch state '{value}'"
            ))),
        }
    }
}

/// What the driver recorded for an item that has no run. Once an item has a
/// run, the run is the only source of truth.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchItemState {
    Pending,
    Failed,
    NotSubmitted,
}

impl BatchItemState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "failed" => Ok(Self::Failed),
            "not_submitted" => Ok(Self::NotSubmitted),
            _ => Err(HarnessError::Database(format!(
                "unknown persisted batch item state '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatchRecord {
    pub id: String,
    pub workflow_id: String,
    pub workflow_revision: String,
    pub request: Value,
    pub item_count: usize,
    pub state: BatchState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct StoredBatchItem {
    pub index: u32,
    pub parameters: Value,
    pub state: BatchItemState,
    pub error: Option<String>,
    pub run: Option<RunRecord>,
}

pub struct NewBatchItem {
    pub parameters: Value,
}

pub struct NewBatch {
    pub id: String,
    pub idempotency_key: String,
    pub fingerprint: String,
    pub workflow_id: String,
    pub workflow_revision: String,
    pub request: Value,
    pub items: Vec<NewBatchItem>,
}

pub struct BatchPage {
    pub batches: Vec<BatchRecord>,
    pub next_cursor: Option<String>,
}

pub(crate) struct BatchLink {
    pub batch_id: String,
    pub index: u32,
}

pub(crate) enum BatchReservation {
    New(BatchRecord),
    Existing(BatchRecord),
}

const NO_RUN: &str = "NOT EXISTS (SELECT 1 FROM runs r WHERE r.batch_id = batch_items.batch_id AND r.batch_index = batch_items.item_index)";

fn map_batch(row: &Row<'_>) -> rusqlite::Result<BatchRecord> {
    let invalid = |error: HarnessError| rusqlite::Error::ToSqlConversionFailure(Box::new(error));
    Ok(BatchRecord {
        id: row.get("id")?,
        workflow_id: row.get("workflow_id")?,
        workflow_revision: row.get("workflow_revision")?,
        request: parse_json_column(row, "request_json")?,
        item_count: row.get::<_, i64>("item_count")? as usize,
        state: BatchState::parse(&row.get::<_, String>("state")?).map_err(invalid)?,
        created_at: parse_sqlite_time(row, row.as_ref().column_index("created_at")?)?,
        updated_at: parse_sqlite_time(row, row.as_ref().column_index("updated_at")?)?,
    })
}

fn load_batch(connection: &Connection, id: &str) -> Result<BatchRecord> {
    connection
        .query_row("SELECT * FROM batches WHERE id = ?1", [id], map_batch)
        .optional()?
        .ok_or_else(|| HarnessError::BatchNotFound {
            batch_id: id.to_owned(),
        })
}

fn find_batch(
    connection: &Connection,
    key: &str,
    fingerprint: &str,
) -> Result<Option<BatchRecord>> {
    let found: Option<(String, String)> = connection
        .query_row(
            "SELECT fingerprint, id FROM batches WHERE idempotency_key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match found {
        Some((stored, id)) if stored == fingerprint => load_batch(connection, &id).map(Some),
        Some(_) => Err(HarnessError::IdempotencyConflict),
        None => Ok(None),
    }
}

impl RunRepository {
    pub(crate) fn find_batch(&self, key: &str, fingerprint: &str) -> Result<Option<BatchRecord>> {
        self.guarded_read(|connection| find_batch(connection, key, fingerprint))
    }

    /// Creates the batch and every item row in one transaction, or reports the
    /// batch that already owns the key.
    pub(crate) fn reserve_batch(&self, batch: &NewBatch) -> Result<BatchReservation> {
        self.guarded(|connection| {
            if let Some(existing) = find_batch(connection, &batch.idempotency_key, &batch.fingerprint)? {
                return Ok(BatchReservation::Existing(existing));
            }
            let now = Utc::now().to_rfc3339();
            connection.execute(
                "INSERT INTO batches (id, idempotency_key, fingerprint, workflow_id, workflow_revision,
                    request_json, item_count, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'submitting', ?8, ?8)",
                params![
                    batch.id,
                    batch.idempotency_key,
                    batch.fingerprint,
                    batch.workflow_id,
                    batch.workflow_revision,
                    serde_json::to_string(&batch.request)?,
                    batch.items.len() as i64,
                    now
                ],
            )?;
            let mut insert = connection.prepare(
                "INSERT INTO batch_items (batch_id, item_index, parameters_json, state)
                 VALUES (?1, ?2, ?3, 'pending')",
            )?;
            for (index, item) in batch.items.iter().enumerate() {
                insert.execute(params![
                    batch.id,
                    index as i64,
                    serde_json::to_string(&item.parameters)?
                ])?;
            }
            drop(insert);
            Ok(BatchReservation::New(load_batch(connection, &batch.id)?))
        })
    }

    pub fn get_batch(&self, id: &str) -> Result<BatchRecord> {
        self.guarded_read(|connection| load_batch(connection, id))
    }

    pub fn batch_items(&self, id: &str) -> Result<Vec<StoredBatchItem>> {
        self.guarded_read(|connection| {
            let mut runs: std::collections::BTreeMap<u32, RunRecord> = connection
                .prepare("SELECT * FROM runs WHERE batch_id = ?1")?
                .query_map([id], map_run)?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|run| Some((run.batch_index?, run)))
                .collect();
            let mut statement = connection.prepare(
                "SELECT item_index, parameters_json, state, error FROM batch_items
                 WHERE batch_id = ?1 ORDER BY item_index",
            )?;
            let items = statement
                .query_map([id], |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        parse_json_column(row, "parameters_json")?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            items
                .into_iter()
                .map(|(index, parameters, state, error)| {
                    Ok(StoredBatchItem {
                        index,
                        parameters,
                        state: BatchItemState::parse(&state)?,
                        error,
                        run: runs.remove(&index),
                    })
                })
                .collect()
        })
    }

    /// One light row per item with a run: (batch, index, status, never sent).
    pub(crate) fn batch_run_summaries(
        &self,
        ids: &[String],
    ) -> Result<Vec<(String, u32, RunStatus, bool)>> {
        self.guarded_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT batch_id, batch_index, status, preflight_sha256 IS NULL FROM runs WHERE batch_id = ?1",
            )?;
            let mut rows = Vec::new();
            for id in ids {
                let found = statement
                    .query_map([id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u32>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (batch, index, status, never_sent) in found {
                    rows.push((batch, index, RunStatus::parse(&status)?, never_sent));
                }
            }
            Ok(rows)
        })
    }

    pub(crate) fn batch_item_states(&self, id: &str) -> Result<Vec<(u32, BatchItemState)>> {
        self.guarded_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT item_index, state FROM batch_items WHERE batch_id = ?1 ORDER BY item_index",
            )?;
            let rows = statement
                .query_map([id], |row| {
                    Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(index, state)| Ok((index, BatchItemState::parse(&state)?)))
                .collect()
        })
    }

    pub fn list_batches(
        &self,
        workflow: Option<&str>,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<BatchPage> {
        let limit = page_limit(limit)?;
        let cursor = cursor.map(RunCursor::decode).transpose()?;
        self.guarded_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT * FROM batches
                 WHERE (?1 IS NULL OR workflow_id = ?1)
                   AND (?2 IS NULL OR (created_at, id) < (?2, ?3))
                 ORDER BY created_at DESC, id DESC LIMIT ?4",
            )?;
            let mut batches = statement
                .query_map(
                    params![
                        workflow,
                        cursor.as_ref().map(|cursor| &cursor.0),
                        cursor.as_ref().map(|cursor| &cursor.1),
                        (limit + 1) as i64
                    ],
                    map_batch,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let next_cursor = if batches.len() > limit {
                batches.pop();
                batches
                    .last()
                    .map(|batch| {
                        RunCursor(batch.created_at.to_rfc3339(), batch.id.clone()).encode()
                    })
                    .transpose()?
            } else {
                None
            };
            Ok(BatchPage {
                batches,
                next_cursor,
            })
        })
    }

    /// Batches whose driver has not settled them.
    pub(crate) fn submitting_batches(&self) -> Result<Vec<String>> {
        self.guarded_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id FROM batches WHERE state = 'submitting' ORDER BY created_at, id",
            )?;
            let ids = statement
                .query_map([], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok(ids)
        })
    }

    /// Re-arms every item that has no run, so a driver can attempt it. Items
    /// with a run are never touched: they replay through their submission key.
    pub(crate) fn resume_batch(&self, id: &str) -> Result<Vec<(u32, Value)>> {
        self.guarded(|connection| {
            load_batch(connection, id)?;
            let now = Utc::now().to_rfc3339();
            connection.execute(
                &format!(
                    "UPDATE batch_items SET state = 'pending', error = NULL
                     WHERE batch_id = ?1 AND state IN ('failed', 'not_submitted') AND {NO_RUN}"
                ),
                [id],
            )?;
            let pending = pending_items(connection, id)?;
            connection.execute(
                "UPDATE batches SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![
                    id,
                    if pending.is_empty() {
                        BatchState::Finished
                    } else {
                        BatchState::Submitting
                    }
                    .as_str(),
                    now
                ],
            )?;
            Ok(pending)
        })
    }

    pub(crate) fn pending_batch_items(&self, id: &str) -> Result<Vec<(u32, Value)>> {
        self.guarded_read(|connection| pending_items(connection, id))
    }

    pub(crate) fn fail_batch_item(&self, id: &str, index: u32, error: &str) -> Result<()> {
        self.guarded(|connection| {
            connection.execute(
                &format!(
                    "UPDATE batch_items SET state = 'failed', error = ?3
                     WHERE batch_id = ?1 AND item_index = ?2 AND state = 'pending' AND {NO_RUN}"
                ),
                params![id, index, error],
            )?;
            Ok(())
        })
    }

    /// Ends a driver: items that were never given a run become `not_submitted`
    /// with `reason`, and the batch is `interrupted` if any item is in that state.
    pub(crate) fn settle_batch(&self, id: &str, reason: &str) -> Result<BatchState> {
        self.guarded(|connection| {
            connection.execute(
                &format!(
                    "UPDATE batch_items SET state = 'not_submitted', error = ?2
                     WHERE batch_id = ?1 AND state = 'pending' AND {NO_RUN}"
                ),
                params![id, reason],
            )?;
            let interrupted: bool = connection.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM batch_items
                     WHERE batch_id = ?1 AND state = 'not_submitted' AND {NO_RUN})"
                ),
                [id],
                |row| row.get(0),
            )?;
            let state = if interrupted {
                BatchState::Interrupted
            } else {
                BatchState::Finished
            };
            connection.execute(
                "UPDATE batches SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, state.as_str(), Utc::now().to_rfc3339()],
            )?;
            Ok(state)
        })
    }
}

fn pending_items(connection: &Connection, id: &str) -> Result<Vec<(u32, Value)>> {
    let mut statement = connection.prepare(&format!(
        "SELECT item_index, parameters_json FROM batch_items
         WHERE batch_id = ?1 AND state = 'pending' AND {NO_RUN} ORDER BY item_index"
    ))?;
    let items = statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                parse_json_column(row, "parameters_json")?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(items)
}

impl AsyncRunRepository {
    pub(crate) async fn find_batch(
        &self,
        key: String,
        fingerprint: String,
    ) -> Result<Option<BatchRecord>> {
        self.call(move |repo| repo.find_batch(&key, &fingerprint))
            .await
    }

    pub(crate) async fn reserve_batch(&self, batch: NewBatch) -> Result<BatchReservation> {
        self.call(move |repo| repo.reserve_batch(&batch)).await
    }

    pub async fn get_batch(&self, id: &str) -> Result<BatchRecord> {
        let id = id.to_owned();
        self.call(move |repo| repo.get_batch(&id)).await
    }

    pub async fn batch_items(&self, id: &str) -> Result<Vec<StoredBatchItem>> {
        let id = id.to_owned();
        self.call(move |repo| repo.batch_items(&id)).await
    }

    pub(crate) async fn batch_run_summaries(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<(String, u32, RunStatus, bool)>> {
        self.call(move |repo| repo.batch_run_summaries(&ids)).await
    }

    pub(crate) async fn batch_item_states(&self, id: &str) -> Result<Vec<(u32, BatchItemState)>> {
        let id = id.to_owned();
        self.call(move |repo| repo.batch_item_states(&id)).await
    }

    pub async fn list_batches(
        &self,
        workflow: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<BatchPage> {
        self.call(move |repo| repo.list_batches(workflow.as_deref(), cursor.as_deref(), limit))
            .await
    }

    pub(crate) async fn submitting_batches(&self) -> Result<Vec<String>> {
        self.call(|repo| repo.submitting_batches()).await
    }

    pub(crate) async fn resume_batch(&self, id: &str) -> Result<Vec<(u32, Value)>> {
        let id = id.to_owned();
        self.call(move |repo| repo.resume_batch(&id)).await
    }

    pub(crate) async fn pending_batch_items(&self, id: &str) -> Result<Vec<(u32, Value)>> {
        let id = id.to_owned();
        self.call(move |repo| repo.pending_batch_items(&id)).await
    }

    pub(crate) async fn fail_batch_item(&self, id: &str, index: u32, error: String) -> Result<()> {
        let id = id.to_owned();
        self.call(move |repo| repo.fail_batch_item(&id, index, &error))
            .await
    }

    pub(crate) async fn settle_batch(&self, id: &str, reason: String) -> Result<BatchState> {
        let id = id.to_owned();
        self.call(move |repo| repo.settle_batch(&id, &reason)).await
    }
}
