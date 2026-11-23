use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{workflow::GenerationMode, HarnessError, Result};

const SCHEMA_VERSION: i64 = 3;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunRecord {
    pub id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub generation_mode: GenerationMode,
    pub status: RunStatus,
    pub parameters: Value,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone)]
pub struct NewRun {
    pub id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_sha256: String,
    pub generation_mode: GenerationMode,
    pub status: RunStatus,
    pub parameters: Value,
    pub output_dir: String,
}

#[derive(Clone)]
pub struct RunRepository {
    connection: Arc<Mutex<Connection>>,
    identity: Option<DatabaseIdentity>,
}

#[derive(Clone)]
struct DatabaseIdentity {
    workspace: String,
    backend: String,
    endpoint: String,
    claim_legacy: bool,
}

impl RunRepository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut connection, path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            identity: None,
        })
    }

    pub fn with_identity(
        mut self,
        workspace: &Path,
        backend: &str,
        endpoint: &str,
        claim_legacy: bool,
    ) -> Result<Self> {
        let workspace = workspace.to_str().ok_or_else(|| {
            HarnessError::Configuration(format!(
                "workspace '{}': use a UTF-8 path to protect run ownership",
                workspace.display()
            ))
        })?;
        let identity = DatabaseIdentity {
            workspace: workspace.to_owned(),
            backend: backend.to_owned(),
            endpoint: endpoint.to_owned(),
            claim_legacy,
        };
        {
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            bind_identity_in_transaction(&transaction, &identity)?;
            transaction.commit()?;
        }
        self.identity = Some(identity);
        Ok(self)
    }

    // The ownership check and each write share a transaction, including the first run.
    fn guarded<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut connection = self.connection()?;
        if let Some(identity) = &self.identity {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            bind_identity_in_transaction(&transaction, identity)?;
            let result = operation(&transaction)?;
            transaction.commit()?;
            Ok(result)
        } else {
            operation(&connection)
        }
    }

    // A read snapshot keeps the ownership check and results consistent without blocking writers.
    fn guarded_read<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut connection = self.connection()?;
        if let Some(identity) = &self.identity {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            check_identity_for_read(&transaction, identity)?;
            let result = operation(&transaction)?;
            transaction.commit()?;
            Ok(result)
        } else {
            operation(&connection)
        }
    }

    pub fn insert(&self, run: &NewRun) -> Result<RunRecord> {
        self.guarded(|connection| {
            let now = Utc::now().to_rfc3339();
            connection.execute(
                r#"
            INSERT INTO runs (
                id, prompt_id, client_id, workflow_id, workflow_version,
                workflow_sha256, generation_mode, status, parameters_json,
                output_dir, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            "#,
                params![
                    run.id,
                    run.prompt_id,
                    run.client_id,
                    run.workflow_id,
                    run.workflow_version,
                    run.workflow_sha256,
                    generation_mode_name(run.generation_mode),
                    run.status.as_str(),
                    serde_json::to_string(&run.parameters)?,
                    run.output_dir,
                    now,
                ],
            )?;
            load_run(connection, &run.id)
        })
    }

    pub fn get(&self, run_id: &str) -> Result<RunRecord> {
        self.guarded_read(|connection| load_run(connection, run_id))
    }

    pub fn list(&self, limit: usize) -> Result<Vec<RunRecord>> {
        self.guarded_read(|connection| {
            let mut statement = connection
                .prepare("SELECT * FROM runs ORDER BY created_at DESC, id DESC LIMIT ?1")?;
            let rows = statement
                .query_map([limit as i64], map_run)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn active(&self) -> Result<Vec<RunRecord>> {
        self.guarded_read(|connection| {
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
        })
    }

    pub fn update_status(
        &self,
        run_id: &str,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<RunRecord> {
        self.guarded(|connection| {
            let now = Utc::now();
            let completed_at = status.is_terminal().then(|| now.to_rfc3339());
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
            if affected == 0 {
                return Err(HarnessError::RunNotFound {
                    run_id: run_id.to_owned(),
                });
            }
            load_run(connection, run_id)
        })
    }

    /// Update a status only if it still matches the observed value.
    /// The returned snapshot is captured atomically with the update.
    pub fn compare_and_set_status(
        &self,
        run_id: &str,
        expected: RunStatus,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<Option<RunRecord>> {
        self.guarded(|connection| {
            let now = Utc::now();
            let completed_at = status.is_terminal().then(|| now.to_rfc3339());
            Ok(connection
                .query_row(
                    r#"
            UPDATE runs
            SET status = ?3, error = ?4, manifest_path = COALESCE(?5, manifest_path),
                updated_at = ?6, completed_at = COALESCE(?7, completed_at)
            WHERE id = ?1 AND status = ?2
            RETURNING *
            "#,
                    params![
                        run_id,
                        expected.as_str(),
                        status.as_str(),
                        error,
                        manifest_path,
                        now.to_rfc3339(),
                        completed_at
                    ],
                    map_run,
                )
                .optional()?)
        })
    }

    pub fn update_prompt_id(&self, run_id: &str, prompt_id: &str) -> Result<RunRecord> {
        self.guarded(|connection| {
            let now = Utc::now().to_rfc3339();
            let affected = connection.execute(
                "UPDATE runs SET prompt_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![run_id, prompt_id, now],
            )?;
            if affected == 0 {
                return Err(HarnessError::RunNotFound {
                    run_id: run_id.to_owned(),
                });
            }
            load_run(connection, run_id)
        })
    }

    pub fn record_event(
        &self,
        run_id: &str,
        event_type: &str,
        message: Option<&str>,
        payload: &Value,
    ) -> Result<()> {
        self.guarded(|connection| {
            connection.execute(
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
        })
    }

    pub fn events(&self, run_id: &str, limit: usize) -> Result<Vec<RunEvent>> {
        self.guarded_read(|connection| {
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
                        payload: serde_json::from_str(&row.get::<_, String>(4)?).map_err(
                            |error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)),
                        )?,
                        created_at: parse_sqlite_time(row, 5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut rows = rows;
            rows.reverse();
            Ok(rows)
        })
    }

    pub fn record_output(&self, output: &OutputRecord) -> Result<()> {
        self.guarded(|connection| {
            connection.execute(
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
        })
    }

    pub fn outputs(&self, run_id: &str) -> Result<Vec<OutputRecord>> {
        self.guarded_read(|connection| {
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
        })
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|error| {
            HarnessError::Database(format!("state database lock was poisoned: {error}"))
        })
    }
}

fn check_identity_for_read(
    transaction: &rusqlite::Transaction<'_>,
    identity: &DatabaseIdentity,
) -> Result<()> {
    let existing: Option<(String, String, String)> = transaction.query_row(
        "SELECT workspace, backend, endpoint FROM genaimage_database_identity WHERE singleton = 1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    ).optional()?;
    if existing
        .as_ref()
        .is_some_and(|(workspace, backend, endpoint)| {
            (workspace.as_str(), backend.as_str(), endpoint.as_str())
                == (
                    identity.workspace.as_str(),
                    identity.backend.as_str(),
                    identity.endpoint.as_str(),
                )
        })
    {
        return Ok(());
    }
    let has_runs: bool =
        transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs)", [], |row| row.get(0))?;
    if !has_runs {
        return Ok(());
    }
    if let Some((workspace, backend, endpoint)) = existing {
        return Err(HarnessError::Configuration(format!("paths.state_database belongs to workspace '{workspace}', backend '{backend}', endpoint '{endpoint}', not workspace '{}', backend '{}', endpoint '{}'; select the original workspace and endpoint or a separate state database", identity.workspace, identity.backend, identity.endpoint)));
    }
    Err(HarnessError::Configuration(format!("paths.state_database contains legacy runs with unknown workspace and endpoint; verify every record belongs to workspace '{}' and endpoint '{}', then set GENAIMAGE_CLAIM_LEGACY_STATE=1 once and reopen the database; otherwise use a separate database", identity.workspace, identity.endpoint)))
}

fn bind_identity_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    identity: &DatabaseIdentity,
) -> Result<()> {
    let existing: Option<(String, String, String)> = transaction.query_row(
        "SELECT workspace, backend, endpoint FROM genaimage_database_identity WHERE singleton = 1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    ).optional()?;
    if existing
        .as_ref()
        .is_some_and(|(workspace, backend, endpoint)| {
            (workspace.as_str(), backend.as_str(), endpoint.as_str())
                == (
                    identity.workspace.as_str(),
                    identity.backend.as_str(),
                    identity.endpoint.as_str(),
                )
        })
    {
        return Ok(());
    }
    let has_runs: bool =
        transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs)", [], |row| row.get(0))?;
    if has_runs {
        if let Some((workspace, backend, endpoint)) = existing {
            return Err(HarnessError::Configuration(format!("paths.state_database belongs to workspace '{workspace}', backend '{backend}', endpoint '{endpoint}', not workspace '{}', backend '{}', endpoint '{}'; select the original workspace and endpoint or a separate state database", identity.workspace, identity.backend, identity.endpoint)));
        }
        if !identity.claim_legacy {
            return Err(HarnessError::Configuration(format!("paths.state_database contains legacy runs with unknown workspace and endpoint; verify every record belongs to workspace '{}' and endpoint '{}', then set GENAIMAGE_CLAIM_LEGACY_STATE=1 once; otherwise use a separate database", identity.workspace, identity.endpoint)));
        }
    }
    transaction.execute("INSERT INTO genaimage_database_identity (singleton, workspace, backend, endpoint) VALUES (1, ?1, ?2, ?3) ON CONFLICT(singleton) DO UPDATE SET workspace=excluded.workspace, backend=excluded.backend, endpoint=excluded.endpoint", params![identity.workspace, identity.backend, identity.endpoint])?;
    Ok(())
}

fn load_run(connection: &Connection, run_id: &str) -> Result<RunRecord> {
    connection
        .query_row("SELECT * FROM runs WHERE id = ?1", [run_id], map_run)
        .optional()?
        .ok_or_else(|| HarnessError::RunNotFound {
            run_id: run_id.to_owned(),
        })
}

// Version 1 is the original run schema. Version 2 also repairs databases from
// older releases that marked version 1 before creating all tables and indexes.
const MIGRATION_1: &str = r#"
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                prompt_id TEXT NOT NULL UNIQUE,
                client_id TEXT NOT NULL,
                workflow_id TEXT NOT NULL,
                workflow_version TEXT NOT NULL,
                workflow_sha256 TEXT NOT NULL,
                generation_mode TEXT NOT NULL,
                status TEXT NOT NULL,
                parameters_json TEXT NOT NULL,
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
"#;
const MIGRATION_3: &str = r#"
    CREATE TABLE IF NOT EXISTS genaimage_database_identity (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        workspace TEXT NOT NULL,
        backend TEXT NOT NULL,
        endpoint TEXT NOT NULL
    );
"#;
const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_1, MIGRATION_3];

// The first version that must already contain each table before migration.
const EXPECTED_TABLES: [(&str, i64, &str); 4] = [
    ("runs", 0, "SELECT id, prompt_id, client_id, workflow_id, workflow_version, workflow_sha256, generation_mode, status, parameters_json, output_dir, manifest_path, error, created_at, updated_at, completed_at FROM runs LIMIT 0"),
    ("run_events", 2, "SELECT id, run_id, event_type, message, payload_json, created_at FROM run_events LIMIT 0"),
    ("outputs", 2, "SELECT id, run_id, node_id, kind, source_filename, source_subfolder, source_type, local_path, sha256, byte_size, created_at FROM outputs LIMIT 0"),
    ("genaimage_database_identity", 3, "SELECT singleton, workspace, backend, endpoint FROM genaimage_database_identity LIMIT 0"),
];

/// Inspect an existing candidate without opening it for writing or migrating it.
/// Ownership is stricter than startup: even an empty database explicitly bound
/// elsewhere may be in use by another process, so the settings API will not rebind it.
pub(crate) fn database_matches_settings(
    path: &Path,
    workspace: &str,
    backend: &str,
    endpoint: &str,
) -> rusqlite::Result<bool> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(1))?;
    connection.pragma_update(None, "trusted_schema", false)?;
    let transaction = connection.transaction()?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let objects: i64 = transaction.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?;
    if version == 0 && objects == 0 {
        return Ok(true);
    }
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut has_identity = false;
    for (table, since, query) in EXPECTED_TABLES {
        let kind: Option<String> = transaction
            .query_row(
                "SELECT type FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .optional()?;
        match kind.as_deref() {
            Some("table") => {
                transaction.prepare(query)?;
            }
            // Legacy migrations may add missing auxiliary or identity tables.
            None if version < since => continue,
            _ => return Err(rusqlite::Error::InvalidQuery),
        }
        has_identity |= table == "genaimage_database_identity";
    }
    let identity: Option<(String, String, String)> = if has_identity {
        transaction.query_row(
            "SELECT workspace, backend, endpoint FROM genaimage_database_identity WHERE singleton = 1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?
    } else {
        None
    };
    if let Some((owner, adapter, url)) = identity {
        Ok((owner.as_str(), adapter.as_str(), url.as_str()) == (workspace, backend, endpoint))
    } else {
        // Unowned legacy runs require an explicit claim outside the settings API.
        transaction.query_row("SELECT NOT EXISTS(SELECT 1 FROM runs)", [], |row| {
            row.get(0)
        })
    }
}

#[cfg(test)]
#[test]
fn inspect_settings_database_rejects_an_unrelated_sqlite_schema() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("other.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert!(
        database_matches_settings(&path, "workspace", "ComfyUI", "http://localhost:8188/").is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

fn migrate(connection: &mut Connection, path: &Path) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(HarnessError::Database(format!(
            "state database '{}' has unsupported schema version {version}; use a compatible GenAImage version",
            path.display()
        )));
    }
    let migration_result = (|| -> rusqlite::Result<()> {
        if version == 0 {
            let objects: i64 = transaction.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [], |row| row.get(0),
            )?;
            let runs: i64 = transaction.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'runs'",
                [],
                |row| row.get(0),
            )?;
            if objects > 0 && runs == 0 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            transaction.execute_batch(migration)?;
            transaction.pragma_update(None, "user_version", (index + 1) as i64)?;
        }
        // Check the columns needed by every operation before accepting a legacy schema.
        for (_, _, query) in EXPECTED_TABLES {
            transaction.prepare(query)?;
        }
        Ok(())
    })();
    migration_result.map_err(|error| HarnessError::Database(format!(
        "could not migrate state database '{}': {error}. Back up the database, then move it aside to rebuild the operational cache; keep the run records on disk",
        path.display()
    )))?;
    transaction.commit()?;
    Ok(())
}

/// An async boundary for the synchronous repository. Each operation runs on
/// Tokio's blocking pool; cancelling a future does not cancel an in-flight write.
#[derive(Clone)]
pub struct AsyncRunRepository {
    repository: RunRepository,
}

impl RunRepository {
    pub fn as_async(&self) -> AsyncRunRepository {
        AsyncRunRepository {
            repository: self.clone(),
        }
    }
}

impl AsyncRunRepository {
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let repository = tokio::task::spawn_blocking(move || RunRepository::open(&path))
            .await
            .map_err(blocking_task_error)??;
        Ok(repository.as_async())
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(RunRepository) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || operation(repository))
            .await
            .map_err(blocking_task_error)?
    }

    pub async fn insert(&self, run: &NewRun) -> Result<RunRecord> {
        let run = run.clone();
        self.call(move |repo| repo.insert(&run)).await
    }

    pub async fn get(&self, run_id: &str) -> Result<RunRecord> {
        let run_id = run_id.to_owned();
        self.call(move |repo| repo.get(&run_id)).await
    }

    pub async fn list(&self, limit: usize) -> Result<Vec<RunRecord>> {
        self.call(move |repo| repo.list(limit)).await
    }

    pub async fn active(&self) -> Result<Vec<RunRecord>> {
        self.call(move |repo| repo.active()).await
    }

    pub async fn update_status(
        &self,
        run_id: &str,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<RunRecord> {
        let run_id = run_id.to_owned();
        let error = error.map(str::to_owned);
        let manifest_path = manifest_path.map(str::to_owned);
        self.call(move |repo| {
            repo.update_status(&run_id, status, error.as_deref(), manifest_path.as_deref())
        })
        .await
    }

    pub async fn compare_and_set_status(
        &self,
        run_id: &str,
        expected: RunStatus,
        status: RunStatus,
        error: Option<&str>,
        manifest_path: Option<&str>,
    ) -> Result<Option<RunRecord>> {
        let run_id = run_id.to_owned();
        let error = error.map(str::to_owned);
        let manifest_path = manifest_path.map(str::to_owned);
        self.call(move |repo| {
            repo.compare_and_set_status(
                &run_id,
                expected,
                status,
                error.as_deref(),
                manifest_path.as_deref(),
            )
        })
        .await
    }

    pub async fn update_prompt_id(&self, run_id: &str, prompt_id: &str) -> Result<RunRecord> {
        let run_id = run_id.to_owned();
        let prompt_id = prompt_id.to_owned();
        self.call(move |repo| repo.update_prompt_id(&run_id, &prompt_id))
            .await
    }

    pub async fn record_event(
        &self,
        run_id: &str,
        event_type: &str,
        message: Option<&str>,
        payload: &Value,
    ) -> Result<()> {
        let run_id = run_id.to_owned();
        let event_type = event_type.to_owned();
        let message = message.map(str::to_owned);
        let payload = payload.clone();
        self.call(move |repo| repo.record_event(&run_id, &event_type, message.as_deref(), &payload))
            .await
    }

    pub async fn events(&self, run_id: &str, limit: usize) -> Result<Vec<RunEvent>> {
        let run_id = run_id.to_owned();
        self.call(move |repo| repo.events(&run_id, limit)).await
    }

    pub async fn record_output(&self, output: &OutputRecord) -> Result<()> {
        let output = output.clone();
        self.call(move |repo| repo.record_output(&output)).await
    }

    pub async fn outputs(&self, run_id: &str) -> Result<Vec<OutputRecord>> {
        let run_id = run_id.to_owned();
        self.call(move |repo| repo.outputs(&run_id)).await
    }
}

fn blocking_task_error(error: tokio::task::JoinError) -> HarnessError {
    HarnessError::Database(format!("state database task failed: {error}"))
}

fn map_run(row: &Row<'_>) -> rusqlite::Result<RunRecord> {
    let completed_at_index = row.as_ref().column_index("completed_at")?;
    Ok(RunRecord {
        id: row.get("id")?,
        prompt_id: row.get("prompt_id")?,
        client_id: row.get("client_id")?,
        workflow_id: row.get("workflow_id")?,
        workflow_version: row.get("workflow_version")?,
        workflow_sha256: row.get("workflow_sha256")?,
        generation_mode: parse_generation_mode(&row.get::<_, String>("generation_mode")?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        status: RunStatus::parse(&row.get::<_, String>("status")?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        parameters: parse_json_column(row, "parameters_json")?,
        output_dir: row.get("output_dir")?,
        manifest_path: row.get("manifest_path")?,
        error: row.get("error")?,
        created_at: parse_sqlite_time(row, row.as_ref().column_index("created_at")?)?,
        updated_at: parse_sqlite_time(row, row.as_ref().column_index("updated_at")?)?,
        completed_at: row
            .get::<_, Option<String>>(completed_at_index)?
            .map(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            completed_at_index,
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
