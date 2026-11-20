use std::{fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

fn write(path: &Path, value: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, value).expect("write file");
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_genaimage"))
}

#[test]
fn workflow_validation_and_character_gates_are_available_through_cli() {
    let temp = TempDir::new().expect("temp");
    write(
        &temp.path().join("genaimage.toml"),
        "[profiles.local]\nbase_url = \"http://127.0.0.1:8188\"\n",
    );
    write(
        &temp.path().join("workflows/comfyui/example/api.json"),
        r#"{
  "3": {
    "class_type": "KSampler",
    "inputs": { "seed": 1 }
  }
}"#,
    );
    write(
        &temp
            .path()
            .join("workflows/comfyui/example/workflow.genaimage.json"),
        r#"{
  "schema_version": 1,
  "id": "example",
  "version": "1.0.0",
  "workflow": "api.json",
  "generation_mode": "generic",
  "parameters": {
    "seed": {
      "node": "3",
      "input": "seed",
      "kind": "seed",
      "required": true,
      "default": 1
    }
  }
}"#,
    );
    let validate = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["workflow", "validate", "example"])
        .output()
        .expect("run CLI validate");
    assert!(
        validate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&validate.stderr)
    );
    let validation: Value = serde_json::from_slice(&validate.stdout).expect("validation JSON");
    assert_eq!(validation["valid"], true);

    let init = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["character", "init", "CHR-CLI", "--name", "CLI character"])
        .output()
        .expect("run character init");
    assert!(
        init.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(temp
        .path()
        .join("characters/CHR-CLI/workflow-state.json")
        .is_file());

    let gate = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args([
            "character",
            "approve",
            "CHR-CLI",
            "--gate",
            "gate1",
            "--approver",
            "test",
            "--note",
            "input complete",
        ])
        .output()
        .expect("run gate approval");
    assert!(
        gate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&gate.stderr)
    );
    let state: Value = serde_json::from_slice(&gate.stdout).expect("gate JSON");
    assert_eq!(state["gates"]["gate1"]["status"], "approved");
}

#[test]
fn invalid_gate_fails_without_mutating_state() {
    let temp = TempDir::new().expect("temp");
    let init = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["character", "init", "CHR-INVALID"])
        .output()
        .expect("run init");
    assert!(init.status.success());

    let premature = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args([
            "character",
            "approve",
            "CHR-INVALID",
            "--gate",
            "gate3",
            "--approver",
            "test",
            "--note",
            "not allowed",
        ])
        .output()
        .expect("run premature approval");
    assert!(!premature.status.success());
    let error: Value = serde_json::from_slice(&premature.stderr).expect("error JSON");
    assert_eq!(error["status"], "error");
    assert!(error["error"]
        .as_str()
        .expect("error string")
        .contains("Gate 1"));
}
