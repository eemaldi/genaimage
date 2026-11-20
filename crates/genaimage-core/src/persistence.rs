use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{workflow::GenerationMode, HarnessError, Result};

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created,
    Submitting,
    UnknownSubmission,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelling,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Submitting => "submitting",
            Self::UnknownSubmission => "unknown_submission",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_trackable(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Submitting | Self::Queued | Self::Running | Self::Cancelling
        )
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "created" => Ok(Self::Created),
            "submitting" => Ok(Self::Submitting),
            "unknown_submission" => Ok(Self::UnknownSubmission),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelling" => Ok(Self::Cancelling),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(HarnessError::Database(format!(
                "unknown persisted run status '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RunRecord {
    pub id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub generation_mode: GenerationMode,
    pub character_id: Option<String>,
    pub status: RunStatus,
    pub parameters: Value,
    pub gate_snapshot: Option<Value>,
    pub output_dir: String,
    pub manifest_path: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunEvent {
    pub id: i64,
    pub run_id: String,
    pub event_type: String,
    pub message: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputRecord {
    pub run_id: String,
    pub node_id: String,
    pub kind: String,
    pub source_filename: String,
    pub source_subfolder: String,
    pub source_type: String,
    pub local_path: String,
    pub sha256: String,
    pub byte_size: u64,
    pub created_at: DateTime<Utc>,
}

pub struct NewRun {
    pub id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub generation_mode: GenerationMode,
    pub character_id: Option<String>,
    pub status: RunStatus,
    pub parameters: Value,
    pub gate_snapshot: Option<Value>,
    pub output_dir: String,
}

#[derive(Clone)]
pub struct RunRepository {
    connection: Arc<Mutex<Connection>>,
}

impl RunRepository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        connection.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                prompt_id TEXT NOT NULL UNIQUE,
                client_id TEXT NOT NULL,
                workflow_id TEXT NOT NULL,
                workflow_version TEXT NOT NULL,
                workflow_sha256 TEXT NOT NULL,
                generation_mode TEXT NOT NULL,
                character_id TEXT,
                status TEXT NOT NULL,
                parameters_json TEXT NOT NULL,
                gate_snapshot_json TEXT,
                output_dir TEXT NOT NULL,
                manifest_path TEXT,
                error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS run_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                event_type TEXT NOT NULL,
                message TEXT,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS run_events_run_id_idx
                ON run_events(run_id, id);
            CREATE TABLE IF NOT EXISTS outputs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                source_filename TEXT NOT NULL,
                source_subfolder TEXT NOT NULL,
                source_type TEXT NOT NULL,
                local_path TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                byte_size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(run_id, source_filename, source_subfolder, source_type)
            );
            CREATE INDEX IF NOT EXISTS outputs_run_id_idx ON outputs(run_id);
            "#,
        )?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn insert(&self, run: &NewRun) -> Result<RunRecord> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO runs (
                id, prompt_id, client_id, workflow_id, workflow_version,
                workflow_sha256, generation_mode, character_id, status,
                parameters_json, gate_snapshot_json, output_dir, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
            "#,
            params![
                run.id,
                run.prompt_id,
                run.client_id,
                run.workflow_id,
                run.workflow_version,
                run.workflow_sha256,
                generation_mode_name(run.generation_mode),
                run.character_id,
                run.status.as_str(),
                serde_json::to_string(&run.parameters)?,
                run.gate_snapshot
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.output_dir,
                now,
            ],
        )?;
        drop(connection);
        self.get(&run.id)
    }

    pub fn get(&self, run_id: &str) -> Result<RunRecord> {
        let connection = self.connection()?;
        connection
            .query_row("SELECT * FROM runs WHERE id = ?1", [run_id], map_run)
            .optional()?
            .ok_or_else(|| HarnessError::RunNotFound {
                run_id: run_id.to_owned(),
            })
    }

    pub fn list(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT * FROM runs ORDER BY created_at DESC, id DESC LIMIT ?1")?;
        let rows = statement
            .query_map([limit as i64], map_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn active(&self) -> Result<Vec<RunRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT * FROM runs
            WHERE status IN (
                'created', 'submitting', 'unknown_submission',
                'queued', 'running', 'cancelling'
            )
            ORDER BY created_at
            "#,
        )?;
        let rows = statement
            .query_map([], map_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_status(
        &self,
        run_id: &str,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<RunRecord> {
        let now = Utc::now();
        let completed_at = status.is_terminal().then(|| now.to_rfc3339());
        let connection = self.connection()?;
        let affected = connection.execute(
            r#"
            UPDATE runs
            SET status = ?2,
                error = ?3,
                manifest_path = COALESCE(?4, manifest_path),
                updated_at = ?5,
                completed_at = COALESCE(?6, completed_at)
            WHERE id = ?1
            "#,
            params![
                run_id,
                status.as_str(),
                error,
                manifest_path,
                now.to_rfc3339(),
                completed_at
            ],
        )?;
        drop(connection);
        if affected == 0 {
            return Err(HarnessError::RunNotFound {
                run_id: run_id.to_owned(),
            });
        }
        self.get(run_id)
    }

    pub fn update_prompt_id(&self, run_id: &str, prompt_id: &str) -> Result<RunRecord> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection()?;
        let affected = connection.execute(
            "UPDATE runs SET prompt_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![run_id, prompt_id, now],
        )?;
        drop(connection);
        if affected == 0 {
            return Err(HarnessError::RunNotFound {
                run_id: run_id.to_owned(),
            });
        }
        self.get(run_id)
    }

    pub fn record_event(
        &self,
        run_id: &str,
        event_type: &str,
        message: Option<&str>,
        payload: &Value,
    ) -> Result<()> {
        self.connection()?.execute(
            r#"
            INSERT INTO run_events (run_id, event_type, message, payload_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                run_id,
                event_type,
                message,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn events(&self, run_id: &str, limit: usize) -> Result<Vec<RunEvent>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, run_id, event_type, message, payload_json, created_at
            FROM run_events
            WHERE run_id = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = statement
            .query_map(params![run_id, limit as i64], |row| {
                Ok(RunEvent {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    event_type: row.get(2)?,
                    message: row.get(3)?,
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                    created_at: parse_sqlite_time(row, 5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }

    pub fn record_output(&self, output: &OutputRecord) -> Result<()> {
        self.connection()?.execute(
            r#"
            INSERT INTO outputs (
                run_id, node_id, kind, source_filename, source_subfolder, source_type,
                local_path, sha256, byte_size, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(run_id, source_filename, source_subfolder, source_type)
            DO UPDATE SET
                node_id = excluded.node_id,
                kind = excluded.kind,
                local_path = excluded.local_path,
                sha256 = excluded.sha256,
                byte_size = excluded.byte_size,
                created_at = excluded.created_at
            "#,
            params![
                output.run_id,
                output.node_id,
                output.kind,
                output.source_filename,
                output.source_subfolder,
                output.source_type,
                output.local_path,
                output.sha256,
                output.byte_size as i64,
                output.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn outputs(&self, run_id: &str) -> Result<Vec<OutputRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT run_id, node_id, kind, source_filename, source_subfolder,
                   source_type, local_path, sha256, byte_size, created_at
            FROM outputs WHERE run_id = ?1 ORDER BY id
            "#,
        )?;
        let rows = statement
            .query_map([run_id], |row| {
                Ok(OutputRecord {
                    run_id: row.get(0)?,
                    node_id: row.get(1)?,
                    kind: row.get(2)?,
                    source_filename: row.get(3)?,
                    source_subfolder: row.get(4)?,
                    source_type: row.get(5)?,
                    local_path: row.get(6)?,
                    sha256: row.get(7)?,
                    byte_size: row.get::<_, i64>(8)? as u64,
                    created_at: parse_sqlite_time(row, 9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|error| {
            HarnessError::Database(format!("state database lock was poisoned: {error}"))
        })
    }
}

fn map_run(row: &Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get("id")?,
        prompt_id: row.get("prompt_id")?,
        client_id: row.get("client_id")?,
        workflow_id: row.get("workflow_id")?,
        workflow_version: row.get("workflow_version")?,
        workflow_sha256: row.get("workflow_sha256")?,
        generation_mode: parse_generation_mode(&row.get::<_, String>("generation_mode")?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        character_id: row.get("character_id")?,
        status: RunStatus::parse(&row.get::<_, String>("status")?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        parameters: parse_json_column(row, "parameters_json")?,
        gate_snapshot: row
            .get::<_, Option<String>>("gate_snapshot_json")?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        output_dir: row.get("output_dir")?,
        manifest_path: row.get("manifest_path")?,
        error: row.get("error")?,
        created_at: parse_sqlite_time(row, 14)?,
        updated_at: parse_sqlite_time(row, 15)?,
        completed_at: row
            .get::<_, Option<String>>("completed_at")?
            .map(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            16,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
            })
            .transpose()?,
    })
}

fn parse_json_column(row: &Row<'_>, column: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(&row.get::<_, String>(column)?)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn parse_sqlite_time(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let value = row.get::<_, String>(index)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn generation_mode_name(mode: GenerationMode) -> &'static str {
    match mode {
        GenerationMode::Generic => "generic",
        GenerationMode::InitialConcept => "initial_concept",
        GenerationMode::Correction => "correction",
        GenerationMode::Refinement => "refinement",
        GenerationMode::MultiView => "multi_view",
        GenerationMode::Delivery => "delivery",
    }
}

fn parse_generation_mode(value: &str) -> std::result::Result<GenerationMode, HarnessError> {
    match value {
        "generic" => Ok(GenerationMode::Generic),
        "initial_concept" => Ok(GenerationMode::InitialConcept),
        "correction" => Ok(GenerationMode::Correction),
        "refinement" => Ok(GenerationMode::Refinement),
        "multi_view" => Ok(GenerationMode::MultiView),
        "delivery" => Ok(GenerationMode::Delivery),
        _ => Err(HarnessError::Database(format!(
            "unknown persisted generation mode '{value}'"
        ))),
    }
}
