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
fn valid_workflows_are_discovered_and_resolved_among_invalid_sidecars() {
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
            .join("workflows/comfyui/example/valid.genaimage.json"),
        r#"{
  "schema_version": 2,
  "id": "example",
  "version": "1.0.0",
  "workflow": "api.json",
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
    let legacy = "workflows/comfyui/example/legacy.genaimage.json";
    write(
        &temp.path().join(legacy),
        r#"{"schema_version":1,"id":"legacy","version":"1.0.0","workflow":"api.json","required_gates":["gate3"]}"#,
    );
    write(
        &temp
            .path()
            .join("workflows/comfyui/example/broken.genaimage.json"),
        "{",
    );

    let list = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["workflow", "list"])
        .output()
        .expect("run CLI list");
    let warnings = String::from_utf8_lossy(&list.stderr);
    assert!(list.status.success(), "stderr: {warnings}");
    let manifests: Vec<Value> = serde_json::from_slice(&list.stdout).expect("manifest list JSON");
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests[0]["id"], "example");
    assert!(warnings.contains("legacy.genaimage.json"));
    assert!(warnings.contains("upgrade to version 2"));
    assert!(warnings.contains("broken.genaimage.json"));

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

    let rejected = cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["workflow", "validate", legacy])
        .output()
        .expect("validate the legacy sidecar explicitly");
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).expect("error JSON");
    let message = error["error"].as_str().expect("error message");
    assert!(message.contains(legacy));
    assert!(message.contains("upgrade to version 2"));
}

#[test]
fn integer_boundaries_are_enforced_by_the_cli() {
    let temp = TempDir::new().expect("temp");
    let workflow_dir = temp.path().join("workflows/comfyui/example");
    write(
        &workflow_dir.join("api.json"),
        r#"{"3":{"class_type":"KSampler","inputs":{"seed":1}}}"#,
    );
    for kind in ["integer", "seed"] {
        for (value, valid) in [
            ("-9223372036854775808", true),
            ("18446744073709551615", true),
            ("-9223372036854775809", false),
            ("18446744073709551616", false),
        ] {
            let manifest = format!(
                r#"{{"schema_version":2,"id":"example","version":"1.0.0","workflow":"api.json","parameters":{{"seed":{{"node":"3","input":"seed","kind":"{kind}","default":{value}}}}}}}"#
            );
            write(&workflow_dir.join("example.genaimage.json"), &manifest);
            let output = cli()
                .arg("--workspace")
                .arg(temp.path())
                .arg("--json")
                .args(["workflow", "validate", "example"])
                .output()
                .expect("run CLI");
            assert_eq!(
                output.status.success(),
                valid,
                "{kind}/{value}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn run_by_id_refuses_to_choose_between_valid_and_legacy_sidecars() {
    let temp = TempDir::new().expect("temp");
    let workflows = temp.path().join("workflows/comfyui");
    write(
        &workflows.join("api.json"),
        r#"{"3":{"class_type":"KSampler","inputs":{"seed":1}}}"#,
    );
    write(
        &workflows.join("valid.genaimage.json"),
        r#"{"schema_version":2,"id":"shared","version":"1.0.0","workflow":"api.json"}"#,
    );
    write(
        &workflows.join("legacy.genaimage.json"),
        r#"{"schema_version":1,"id":"shared","version":"1.0.0","workflow":"api.json","required_gates":["gate3"]}"#,
    );
    let run_cli = |args: &[&str]| {
        cli()
            .arg("--workspace")
            .arg(temp.path())
            .arg("--json")
            .args(args)
            .output()
            .expect("run CLI")
    };
    for valid_name in ["valid.genaimage.json", "shared.genaimage.json"] {
        let rejected = run_cli(&["run", "shared"]);
        assert!(!rejected.status.success());
        let error: Value = serde_json::from_slice(&rejected.stderr).expect("error JSON");
        let message = error["error"].as_str().expect("error message");
        assert!(message.contains("workflow id 'shared'"), "{message}");
        assert!(message.contains("legacy.genaimage.json"), "{message}");
        assert!(
            message.contains("select an explicit manifest path"),
            "{message}"
        );
        assert!(run_cli(&["workflow", "validate", valid_name])
            .status
            .success());

        if valid_name == "valid.genaimage.json" {
            fs::rename(
                workflows.join(valid_name),
                workflows.join("shared.genaimage.json"),
            )
            .expect("move valid sidecar to the conventional filename");
        }
    }
}

#[test]
fn jobs_accepts_the_retired_workspace_path_but_rejects_other_unknown_keys() {
    let temp = TempDir::new().expect("temp");
    let config = temp.path().join("genaimage.toml");
    let legacy = "[paths]\ncharacters = \"characters\"\n";
    write(&config, legacy);
    let jobs = |command| {
        cli()
            .arg("--workspace")
            .arg(temp.path())
            .arg("--json")
            .args(["jobs", command])
            .output()
            .expect("run CLI jobs command")
    };
    for command in ["list", "reconcile"] {
        let output = jobs(command);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("jobs JSON");
        assert!(runs.is_empty());
    }

    write(&config, &format!("{legacy}typo = \"invalid\"\n"));
    let rejected = jobs("list");
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).expect("error JSON");
    assert!(error["error"]
        .as_str()
        .expect("error message")
        .contains("unknown field `typo`"));
}
