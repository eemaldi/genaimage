use std::time::{Duration, Instant};

use genaimage_core::{
    persistence::{AsyncRunRepository, NewRun, RunRepository},
    workflow::GenerationMode,
    RunStatus,
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn new_run() -> NewRun {
    NewRun {
        id: "run".into(),
        prompt_id: "prompt".into(),
        client_id: "client".into(),
        workflow_id: "workflow".into(),
        workflow_version: "1".into(),
        workflow_sha256: "hash".into(),
        generation_mode: GenerationMode::Generic,
        status: RunStatus::Queued,
        parameters: json!({}),
        output_dir: "runs/run".into(),
    }
}

#[test]
fn fresh_database_has_no_gate_columns_and_enforces_foreign_keys() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let repository = RunRepository::open(&path).unwrap();
    assert!(repository
        .record_event("missing", "event", None, &json!({}))
        .is_err());
    let conn = Connection::open(&path).unwrap();
    let mut statement = conn.prepare("PRAGMA table_info(runs)").unwrap();
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!columns
        .iter()
        .any(|name| name.contains("gate") || name == "character_id"));
}

#[test]
fn unversioned_legacy_database_is_migrated_without_losing_runs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        &include_str!("fixtures/legacy-runs.sql").replace("PRAGMA user_version = 1;", ""),
    )
    .unwrap();
    drop(conn);

    let repository = RunRepository::open(&path).unwrap();
    assert_eq!(
        repository.get("run-legacy").unwrap().status,
        RunStatus::Completed
    );
    repository.insert(&new_run()).unwrap();
    repository
        .record_event("run", "queued", None, &json!({}))
        .unwrap();
    drop(repository);
    let reopened = RunRepository::open(&path).unwrap();
    assert_eq!(reopened.list(10).unwrap().len(), 2);
    assert_eq!(reopened.events("run", 10).unwrap().len(), 1);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
}

#[test]
fn incompatible_or_newer_database_retains_data_and_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE runs (id TEXT); INSERT INTO runs VALUES ('keep');")
        .unwrap();
    drop(conn);
    let error = RunRepository::open(&path).err().unwrap().to_string();
    assert!(
        error.contains("Back up the database") && error.contains("move it aside"),
        "{error}"
    );
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT id FROM runs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='run_events'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.pragma_update(None, "user_version", 999).unwrap();
    drop(conn);
    let error = RunRepository::open(&path).err().unwrap().to_string();
    assert!(error.contains("unsupported schema version 999"), "{error}");
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
    assert_eq!(
        conn.query_row("SELECT id FROM runs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_connections_can_read_and_write() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let first = AsyncRunRepository::open(&path).await.unwrap();
    let second = AsyncRunRepository::open(&path).await.unwrap();
    first.insert(&new_run()).await.unwrap();
    let mut tasks = Vec::new();
    for writer in 0..4 {
        let repo = if writer % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        tasks.push(tokio::spawn(async move {
            for i in 0..40 {
                repo.record_event("run", "progress", None, &json!({"writer": writer, "i": i}))
                    .await
                    .unwrap();
                assert_eq!(repo.get("run").await.unwrap().id, "run");
                let _ = repo.events("run", 10).await.unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(first.events("run", 200).await.unwrap().len(), 160);
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_for_a_sqlite_writer_does_not_block_the_runtime() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let repository = AsyncRunRepository::open(&path).await.unwrap();
    repository.insert(&new_run()).await.unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        conn.execute_batch("COMMIT").unwrap();
    });
    ready_rx.recv().unwrap();
    let writer = tokio::spawn(async move {
        repository
            .record_event("run", "progress", None, &json!({}))
            .await
    });
    let start = Instant::now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        start.elapsed() < Duration::from_millis(700),
        "SQLite blocked the runtime"
    );
    writer.await.unwrap().unwrap();
    lock_thread.join().unwrap();
}

#[test]
fn empty_database_can_rebind_but_stale_handles_cannot_use_foreign_runs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let first = RunRepository::open(&path)
        .unwrap()
        .with_identity(dir.path(), "ComfyUI", "http://localhost:8188", false)
        .unwrap();
    let second = RunRepository::open(&path)
        .unwrap()
        .with_identity(dir.path(), "ComfyUI", "http://localhost:9999", false)
        .unwrap();
    assert!(second.list(10).unwrap().is_empty());
    first.insert(&new_run()).unwrap();
    for result in [
        second.get("run").map(|_| ()),
        second.list(10).map(|_| ()),
        second
            .update_status("run", RunStatus::Cancelled, None, None)
            .map(|_| ()),
    ] {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("separate state database"));
    }
    let mut second_run = new_run();
    second_run.id = "another".into();
    second_run.prompt_id = "other-prompt".into();
    assert!(second.insert(&second_run).is_err());
    assert_eq!(first.get("run").unwrap().status, RunStatus::Queued);
    assert_eq!(first.list(10).unwrap().len(), 1);
}

#[test]
fn competing_first_runs_bind_atomically() {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let first = RunRepository::open(&path)
        .unwrap()
        .with_identity(dir.path(), "ComfyUI", "http://localhost:8188", false)
        .unwrap();
    let second = RunRepository::open(&path)
        .unwrap()
        .with_identity(dir.path(), "ComfyUI", "http://localhost:9999", false)
        .unwrap();
    let gate = Arc::new(Barrier::new(3));
    let start = |repo: RunRepository, id: &str| {
        let gate = gate.clone();
        let mut run = new_run();
        run.id = id.into();
        run.prompt_id = id.into();
        thread::spawn(move || {
            gate.wait();
            repo.insert(&run)
        })
    };
    let a = start(first, "first");
    let b = start(second, "second");
    gate.wait();
    let a = a.join().unwrap();
    let b = b.join().unwrap();
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "exactly one endpoint must own the first run"
    );
    assert!(a
        .err()
        .or_else(|| b.err())
        .unwrap()
        .to_string()
        .contains("separate state database"));
    assert_eq!(
        RunRepository::open(&path).unwrap().list(10).unwrap().len(),
        1
    );
}

#[test]
fn guarded_reads_remain_available_during_a_wal_write() {
    use std::{sync::mpsc, thread};
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.sqlite3");
    let repo = RunRepository::open(&path)
        .unwrap()
        .with_identity(dir.path(), "ComfyUI", "http://localhost:8188", false)
        .unwrap();
    repo.insert(&new_run()).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE runs SET status = 'running' WHERE id = 'run';")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || sender.send(repo.list(10)).unwrap());
    let result = receiver.recv_timeout(Duration::from_secs(2));
    writer.execute_batch("ROLLBACK").unwrap();
    reader.join().unwrap();
    assert_eq!(
        result.expect("read should not wait for writer").unwrap()[0].status,
        RunStatus::Queued
    );
}
