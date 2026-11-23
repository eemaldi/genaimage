use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    comfyui::{ComfyUiClient, HistorySnapshot},
    persistence::{OutputRecord, RunRecord, RunRepository},
    workflow::BoundWorkflow,
    HarnessError, Result,
};

const RUN_MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InputFileRecord {
    pub parameter: String,
    pub source_path: String,
    pub sha256: String,
    pub byte_size: u64,
    pub uploaded_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunManifest {
    pub schema_version: u32,
    pub run_id: String,
    pub prompt_id: String,
    pub client_id: String,
    pub workflow: WorkflowManifestRecord,
    pub preset: Option<String>,
    pub parameters: Value,
    pub comfyui_base_url: String,
    pub source_inputs: Vec<InputFileRecord>,
    pub outputs: Vec<OutputRecord>,
    pub created_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub comfyui_history: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowManifestRecord {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub manifest_sha256: String,
    pub manifest_path: String,
}

pub async fn initialise_run_files(
    output_dir: &Path,
    bound: &BoundWorkflow,
    workflow_manifest_path: &Path,
    preset: Option<&str>,
    parameters: &Value,
    inputs: &[InputFileRecord],
) -> Result<()> {
    create_new_atomic(
        &output_dir.join("workflow-manifest.json"),
        fs::read(workflow_manifest_path)?.as_slice(),
    )?;
    create_new_atomic(
        &output_dir.join("resolved-workflow.json"),
        serde_json::to_vec_pretty(&bound.workflow)?.as_slice(),
    )?;
    let preflight = serde_json::json!({
        "schema_version": 1,
        "workflow_manifest_path": workflow_manifest_path.display().to_string(),
        "preset": preset,
        "parameters": parameters,
        "source_inputs": inputs
    });
    create_new_atomic(
        &output_dir.join("preflight.json"),
        serde_json::to_vec_pretty(&preflight)?.as_slice(),
    )
}

pub async fn materialise_history(
    client: &ComfyUiClient,
    repository: &RunRepository,
    run: &RunRecord,
    workflow_manifest_path: &Path,
    source_inputs: Vec<InputFileRecord>,
    history: &HistorySnapshot,
) -> Result<PathBuf> {
    let output_dir = PathBuf::from(&run.output_dir);
    fs::create_dir_all(output_dir.join("outputs"))?;
    let manifest_path = output_dir.join("run-manifest.json");
    if manifest_path.exists() {
        return Ok(manifest_path);
    }
    let mut output_records = Vec::new();
    for (index, output) in history.outputs.iter().enumerate() {
        let bytes = client.download_output(output).await?;
        let file_name = output_file_name(index, output.kind.as_str(), output.filename.as_str());
        let target = output_dir.join("outputs").join(file_name);
        if target.exists() {
            let existing = fs::read(&target)?;
            if existing != bytes {
                return Err(HarnessError::InvalidPath {
                    path: target,
                    reason: "refusing to overwrite an existing run output".to_owned(),
                });
            }
        } else {
            write_atomic_new(&target, &bytes)?;
        }
        let record = OutputRecord {
            run_id: run.id.clone(),
            node_id: output.node_id.clone(),
            kind: output.kind.clone(),
            source_filename: output.filename.clone(),
            source_subfolder: output.subfolder.clone(),
            source_type: output.output_type.clone(),
            local_path: target.display().to_string(),
            sha256: sha256_hex(&bytes),
            byte_size: bytes.len() as u64,
            created_at: Utc::now(),
        };
        repository.record_output(&record)?;
        output_records.push(record);
    }

    let completed_at = Utc::now();
    let manifest = RunManifest {
        schema_version: RUN_MANIFEST_VERSION,
        run_id: run.id.clone(),
        prompt_id: run.prompt_id.clone(),
        client_id: run.client_id.clone(),
        workflow: WorkflowManifestRecord {
            id: run.workflow_id.clone(),
            version: run.workflow_version.clone(),
            sha256: run.workflow_sha256.clone(),
            manifest_sha256: sha256_hex(&fs::read(output_dir.join("workflow-manifest.json"))?),
            manifest_path: workflow_manifest_path.display().to_string(),
        },
        preset: read_preflight_field(&output_dir, "preset")?
            .and_then(|value| value.as_str().map(str::to_owned)),
        parameters: run.parameters.clone(),
        comfyui_base_url: client.base_url().to_string(),
        source_inputs,
        outputs: output_records,
        created_at: run.created_at,
        completed_at,
        comfyui_history: history.raw.clone(),
    };
    create_new_or_equal(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest)?.as_slice(),
    )?;
    Ok(manifest_path)
}

pub fn checksum_file(path: &Path) -> Result<InputFileRecord> {
    let bytes = fs::read(path)?;
    Ok(InputFileRecord {
        parameter: String::new(),
        source_path: path.display().to_string(),
        sha256: sha256_hex(&bytes),
        byte_size: bytes.len() as u64,
        uploaded_name: String::new(),
    })
}

fn output_file_name(index: usize, kind: &str, source: &str) -> String {
    let extension = Path::new(source)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or("bin");
    let safe_kind: String = kind
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("{:03}-{}.{}", index + 1, safe_kind, extension)
}

fn read_preflight_field(output_dir: &Path, field: &str) -> Result<Option<Value>> {
    let value: Value = serde_json::from_slice(&fs::read(output_dir.join("preflight.json"))?)?;
    Ok(value.get(field).cloned())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn create_new_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        return Err(HarnessError::InvalidPath {
            path: path.to_path_buf(),
            reason: "file already exists".to_owned(),
        });
    }
    write_atomic_new(path, content)
}

fn create_new_or_equal(path: &Path, content: &[u8]) -> Result<()> {
    if path.exists() {
        if fs::read(path)? != content {
            return Err(HarnessError::InvalidPath {
                path: path.to_path_buf(),
                reason: "refusing to replace an existing manifest".to_owned(),
            });
        }
        return Ok(());
    }
    write_atomic_new(path, content)
}

fn write_atomic_new(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| HarnessError::InvalidPath {
        path: path.to_path_buf(),
        reason: "file path has no parent".to_owned(),
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&temporary, content)?;
    if path.exists() {
        fs::remove_file(&temporary)?;
        return Err(HarnessError::InvalidPath {
            path: path.to_path_buf(),
            reason: "file already exists".to_owned(),
        });
    }
    fs::rename(&temporary, path)?;
    Ok(())
}
