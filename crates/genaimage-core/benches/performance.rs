use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use genaimage_core::{
    persistence::{NewRun, RunRepository},
    workflow::{GenerationMode, LoadedWorkflow},
    RunStatus,
};
use serde_json::{json, Value};
use tempfile::{Builder, TempDir};

fn benchmark_temp_dir() -> TempDir {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("find workspace root");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace_root.join(path)
            }
        })
        .unwrap_or_else(|| workspace_root.join("target"));
    fs::create_dir_all(&target_dir).expect("create benchmark target directory");
    Builder::new()
        .prefix("genaimage-bench-")
        .tempdir_in(target_dir)
        .expect("create temporary benchmark directory")
}

fn workflow_binding(c: &mut Criterion) {
    let temp_dir = benchmark_temp_dir();
    let manifest_path = temp_dir.path().join("workflow.genaimage.json");
    let workflow_path = temp_dir.path().join("workflow.json");
    let manifest = json!({
        "schema_version": 2,
        "id": "benchmark.workflow",
        "version": "1",
        "workflow": "workflow.json",
        "parameters": {
            "prompt": {
                "node": "1",
                "input": "text",
                "kind": "string",
                "required": true
            },
            "steps": {
                "node": "2",
                "input": "steps",
                "kind": "integer",
                "default": 20
            },
            "seed": {
                "node": "2",
                "input": "seed",
                "kind": "seed",
                "default": 42
            }
        },
        "presets": {
            "fast": { "steps": 12 }
        }
    });
    let workflow = json!({
        "1": { "class_type": "CLIPTextEncode", "inputs": { "text": "" } },
        "2": { "class_type": "KSampler", "inputs": { "steps": 20, "seed": 0 } }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("serialise benchmark manifest"),
    )
    .expect("write benchmark manifest");
    fs::write(
        &workflow_path,
        serde_json::to_vec(&workflow).expect("serialise benchmark workflow"),
    )
    .expect("write benchmark workflow");

    let loaded = LoadedWorkflow::load(&manifest_path).expect("load benchmark workflow");
    let overrides = BTreeMap::from([(
        "prompt".to_owned(),
        Value::String("benchmark prompt".to_owned()),
    )]);
    c.bench_function("workflow_binding", |b| {
        b.iter(|| black_box(loaded.bind(Some("fast"), black_box(&overrides)).unwrap()))
    });
    drop(temp_dir);
}

fn run_persistence(c: &mut Criterion) {
    let temp_dir = benchmark_temp_dir();
    let repository = RunRepository::open(&temp_dir.path().join("runs.sqlite3"))
        .expect("open benchmark database");
    let next_id = AtomicU64::new(0);
    c.bench_function("run_persistence_insert", |b| {
        b.iter_batched(
            || {
                let id = next_id.fetch_add(1, Ordering::Relaxed).to_string();
                NewRun {
                    id: format!("benchmark-{id}"),
                    prompt_id: format!("prompt-{id}"),
                    client_id: "criterion".to_owned(),
                    workflow_id: "benchmark.workflow".to_owned(),
                    workflow_version: "1".to_owned(),
                    workflow_sha256: "benchmark".to_owned(),
                    workflow_manifest_sha256: None,
                    rerun_of: None,
                    generation_mode: GenerationMode::Generic,
                    status: RunStatus::Queued,
                    parameters: json!({"prompt": "benchmark prompt", "steps": 12}),
                    output_dir: format!("runs/benchmark-{id}"),
                }
            },
            |run| black_box(repository.insert(black_box(&run)).unwrap()),
            BatchSize::SmallInput,
        )
    });
    drop(repository);
    drop(temp_dir);
}

criterion_group!(benches, workflow_binding, run_persistence);
criterion_main!(benches);
