use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use genaimage_core::{
    artifacts::RunManifest,
    persistence::{NewRun, RunRepository},
    workflow::{GenerationMode, LoadedWorkflow},
    RunStatus,
};
use serde_json::{json, Value};
use tempfile::TempDir;

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

fn valid_workflow() -> Value {
    serde_json::from_str(include_str!("fixtures/contract-workflow.api.json"))
        .expect("workflow fixture")
}

fn valid_manifest(workflow: &str) -> Value {
    let mut manifest: Value =
        serde_json::from_str(include_str!("fixtures/contract-workflow.genaimage.json"))
            .expect("manifest fixture");
    manifest["workflow"] = json!(workflow);
    manifest
}

fn create_workflow(temp: &TempDir, manifest: Value, workflow: Value) -> PathBuf {
    let manifest_path = temp.path().join("workflows/test.genaimage.json");
    write_file(
        &manifest_path.with_file_name("workflow.api.json"),
        &serde_json::to_string_pretty(&workflow).expect("workflow JSON"),
    );
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    );
    manifest_path
}

#[test]
fn workflow_manifest_validates_and_applies_typed_parameters() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(&temp, valid_manifest("workflow.api.json"), valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid workflow");
    let bound = loaded
        .bind(
            Some("fast"),
            &BTreeMap::from([("seed".to_owned(), json!(99))]),
        )
        .expect("bound workflow");

    assert_eq!(bound.workflow["3"]["inputs"]["seed"], json!(99));
    assert_eq!(bound.workflow["3"]["inputs"]["steps"], json!(10));
}

#[test]
fn bundled_workflow_manifests_match_the_published_schema() {
    fn collect_manifests(directory: &Path, manifests: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).expect("read workflows directory") {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                collect_manifests(&path, manifests);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".genaimage.json"))
            {
                manifests.push(path);
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let schema: Value = serde_json::from_slice(
        &fs::read(root.join("docs/schemas/workflow-manifest.v2.json")).expect("read schema"),
    )
    .expect("schema JSON");
    let validator = jsonschema::JSONSchema::compile(&schema).expect("compile schema");
    let mut manifests = Vec::new();
    collect_manifests(&root.join("workflows"), &mut manifests);
    assert!(!manifests.is_empty(), "no bundled manifests found");
    for path in manifests {
        let manifest: Value = serde_json::from_slice(&fs::read(&path).expect("read manifest"))
            .expect("manifest JSON");
        if let Err(errors) = validator.validate(&manifest) {
            panic!(
                "{}: {}",
                path.display(),
                errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
            );
        }
        LoadedWorkflow::load(&path).expect("bundled workflow must load");
    }
}

#[test]
fn schema_matches_v2_structure_and_rejects_retired_or_mistyped_fields() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/schemas/workflow-manifest.v2.json"
    ))
    .expect("schema JSON");
    let validator = jsonschema::JSONSchema::compile(&schema).expect("compile schema");
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["parameters"]["seed"]["widget"] = json!("slider");
    manifest["parameters"]["seed"]["step"] = json!(1);
    manifest["parameters"]["seed"]["options"] = json!([1, 2]);
    manifest["parameters"]["seed"]["group"] = json!("Sampling");
    manifest["parameters"]["seed"]["advanced"] = json!(true);
    manifest["parameters"]["seed"]["placeholder"] = json!("seed");
    assert!(validator.is_valid(&manifest));
    let parsed: genaimage_core::workflow::WorkflowManifest =
        serde_json::from_value(manifest.clone()).unwrap();
    assert!(validator.is_valid(&serde_json::to_value(parsed).unwrap()));
    for (field, value) in [
        ("schema_version", json!(1)),
        ("generation_mode", json!("generic")),
        ("required_gates", json!([])),
        ("tags", json!("wrong")),
    ] {
        let mut bad = manifest.clone();
        bad[field] = value;
        assert!(!validator.is_valid(&bad), "accepted {field}");
    }
    for (field, value) in [
        ("widget", json!(1)),
        ("step", json!("one")),
        ("options", json!(false)),
        ("group", json!([])),
        ("advanced", json!("yes")),
        ("placeholder", json!(1)),
        ("unknown", json!(true)),
    ] {
        let mut bad = manifest.clone();
        bad["parameters"]["seed"][field] = value;
        assert!(!validator.is_valid(&bad), "accepted {field}");
        assert!(
            serde_json::from_value::<genaimage_core::workflow::WorkflowManifest>(bad).is_err(),
            "deserialised {field}"
        );
    }
}

#[test]
fn schema_and_loader_enforce_kind_specific_hints_and_defaults() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/schemas/workflow-manifest.v2.json"
    ))
    .unwrap();
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    let temp = TempDir::new().unwrap();
    for (kind, valid, invalid, widgets, step) in [
        (
            "string",
            json!("hello"),
            json!(1),
            vec!["text", "textarea", "select"],
            Value::Null,
        ),
        (
            "image_upload",
            json!("image.png"),
            json!(false),
            vec!["file"],
            Value::Null,
        ),
        (
            "integer",
            json!(5),
            json!(1.5),
            vec!["number", "slider", "select"],
            json!(1),
        ),
        (
            "seed",
            json!(42),
            json!("wrong"),
            vec!["number", "slider", "select"],
            json!(2),
        ),
        (
            "number",
            json!(1.5),
            json!("wrong"),
            vec!["number", "slider", "select"],
            json!(0.5),
        ),
        (
            "boolean",
            json!(true),
            json!(1),
            vec!["checkbox", "select"],
            Value::Null,
        ),
    ] {
        let mut manifest = valid_manifest("workflow.api.json");
        let seed = &mut manifest["parameters"]["seed"];
        seed["kind"] = json!(kind);
        seed["default"] = valid.clone();
        seed["options"] = json!([valid]);
        seed["step"] = step;
        manifest["presets"]["fast"]
            .as_object_mut()
            .unwrap()
            .remove("seed");
        for widget in widgets {
            manifest["parameters"]["seed"]["widget"] = json!(widget);
            assert!(
                validator.is_valid(&manifest),
                "schema rejected {kind}/{widget}"
            );
            let path = create_workflow(&temp, manifest.clone(), valid_workflow());
            assert!(
                LoadedWorkflow::load(&path).is_ok(),
                "loader rejected {kind}/{widget}"
            );
        }
        for field in ["default", "options"] {
            let mut bad = manifest.clone();
            bad["parameters"]["seed"][field] = if field == "options" {
                json!([invalid])
            } else {
                invalid.clone()
            };
            assert!(!validator.is_valid(&bad), "schema accepted {kind}/{field}");
            let path = create_workflow(&temp, bad, valid_workflow());
            assert!(
                LoadedWorkflow::load(&path).is_err(),
                "loader accepted {kind}/{field}"
            );
        }
    }

    for (field, value) in [
        ("options", json!(["wrong", true, {}])),
        ("options", json!([])),
        ("step", json!(-5)),
        ("step", json!(0)),
        ("step", json!(0.5)),
        ("widget", json!("checkbox")),
        ("widget", json!("unknown")),
    ] {
        let mut manifest = valid_manifest("workflow.api.json");
        manifest["parameters"]["seed"][field] = value;
        assert!(!validator.is_valid(&manifest), "schema accepted {field}");
        let path = create_workflow(&temp, manifest, valid_workflow());
        let error = LoadedWorkflow::load(&path).unwrap_err().to_string();
        assert!(
            error.contains(if field == "options" { "option" } else { field }),
            "{error}"
        );
    }
    for kind in ["string", "boolean", "image_upload"] {
        let mut manifest = valid_manifest("workflow.api.json");
        manifest["parameters"]["seed"]["kind"] = json!(kind);
        manifest["parameters"]["seed"]["step"] = json!(1);
        manifest["presets"]["fast"]
            .as_object_mut()
            .unwrap()
            .remove("seed");
        assert!(!validator.is_valid(&manifest));
        let path = create_workflow(&temp, manifest, valid_workflow());
        assert!(LoadedWorkflow::load(&path)
            .unwrap_err()
            .to_string()
            .contains("step"));
    }
    for invalid in [Value::Null, json!([]), json!({})] {
        let mut manifest = valid_manifest("workflow.api.json");
        manifest["presets"]["fast"]["steps"] = invalid;
        assert!(!validator.is_valid(&manifest));
        let path = create_workflow(&temp, manifest, valid_workflow());
        assert!(LoadedWorkflow::load(&path).is_err());
    }
}

#[test]
fn cross_field_rules_require_workflow_validation() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/schemas/workflow-manifest.v2.json"
    ))
    .unwrap();
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    let temp = TempDir::new().unwrap();
    let mut cases = Vec::new();
    let mut default_above_max = valid_manifest("workflow.api.json");
    default_above_max["parameters"]["steps"]["default"] = json!(999);
    cases.push((default_above_max, "above maximum"));
    let mut wrong_preset_type = valid_manifest("workflow.api.json");
    wrong_preset_type["presets"]["fast"]["steps"] = json!("12");
    cases.push((wrong_preset_type, "expects Integer"));
    let mut default_not_in_options = valid_manifest("workflow.api.json");
    default_not_in_options["parameters"]["steps"]["default"] = json!(5);
    default_not_in_options["parameters"]["steps"]["options"] = json!([10, 20]);
    cases.push((default_not_in_options, "default must appear"));
    let mut option_out_of_range = valid_manifest("workflow.api.json");
    option_out_of_range["parameters"]["steps"]["options"] = json!([101]);
    cases.push((option_out_of_range, "above maximum"));
    let mut option_disallowed = valid_manifest("workflow.api.json");
    option_disallowed["parameters"]["steps"]["allowed_values"] = json!([10]);
    option_disallowed["parameters"]["steps"]["options"] = json!([11]);
    cases.push((option_disallowed, "allowed set"));
    for (manifest, expected) in cases {
        assert!(
            validator.is_valid(&manifest),
            "cross-field case unexpectedly expressible in schema"
        );
        let path = create_workflow(&temp, manifest, valid_workflow());
        let error = LoadedWorkflow::load(&path).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn parameter_kinds_still_enforce_their_value_types() {
    use genaimage_core::workflow::ParameterDefinition;
    for (kind, valid, invalid) in [
        ("string", json!("text"), json!(1)),
        ("image_upload", json!("image.png"), json!(true)),
        ("integer", json!(5), json!(5.5)),
        ("seed", json!(42), json!("42")),
        ("number", json!(1.5), json!("1.5")),
        ("boolean", json!(true), json!(1)),
    ] {
        let definition: ParameterDefinition =
            serde_json::from_value(json!({"node": "1", "input": "value", "kind": kind})).unwrap();
        assert!(definition.validate_value("value", &valid).is_ok(), "{kind}");
        assert!(
            definition.validate_value("value", &invalid).is_err(),
            "{kind}"
        );
    }
}

#[test]
fn v2_metadata_and_hints_round_trip_without_changing_bindings() {
    let temp = TempDir::new().expect("temp directory");
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["description"] = json!("A description");
    manifest["tags"] = json!(["one", "two"]);
    manifest["category"] = json!("examples");
    manifest["thumbnail"] = json!("preview.png");
    let seed = &mut manifest["parameters"]["seed"];
    seed["default"] = json!(7);
    seed["widget"] = json!("slider");
    seed["step"] = json!(4.0);
    seed["options"] = json!([7, 42]);
    seed["group"] = json!("Sampling");
    seed["advanced"] = json!(true);
    seed["placeholder"] = json!("Choose a seed");
    let path = create_workflow(&temp, manifest, valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid metadata and hints");
    let serialised = serde_json::to_value(&loaded.manifest).expect("serialise v2");
    assert_eq!(serialised["description"], "A description");
    assert_eq!(serialised["tags"], json!(["one", "two"]));
    assert_eq!(serialised["category"], "examples");
    assert_eq!(serialised["thumbnail"], "preview.png");
    let original = loaded_json(&path);
    for field in [
        "widget",
        "step",
        "options",
        "group",
        "advanced",
        "placeholder",
    ] {
        assert_eq!(
            serialised["parameters"]["seed"][field],
            original["parameters"]["seed"][field]
        );
    }
    let bound = loaded
        .bind(None, &BTreeMap::from([("steps".to_owned(), json!(5))]))
        .expect("defaults");
    assert_eq!(bound.parameters["seed"], 7);
    let bound = loaded
        .bind(
            Some("fast"),
            &BTreeMap::from([("seed".to_owned(), json!(99))]),
        )
        .expect("preset and override");
    assert_eq!(bound.parameters["seed"], 99);
    assert_eq!(bound.parameters["steps"], 10);
    assert!(loaded
        .bind(None, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("required parameter 'steps'"));
}

fn loaded_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read manifest")).expect("manifest JSON")
}

#[test]
fn manifest_versions_and_unknown_fields_are_rejected() {
    let temp = TempDir::new().expect("temp directory");
    for legacy_fields in [
        json!({}),
        json!({"generation_mode": "generic", "required_gates": ["gate3"]}),
    ] {
        let mut manifest = valid_manifest("workflow.api.json");
        manifest["schema_version"] = json!(1);
        manifest
            .as_object_mut()
            .unwrap()
            .extend(legacy_fields.as_object().unwrap().clone());
        let path = create_workflow(&temp, manifest, valid_workflow());
        let error = LoadedWorkflow::load(&path).unwrap_err().to_string();
        assert!(
            error.contains("schema version 1") && error.contains("upgrade to version 2"),
            "{error}"
        );
    }
    for field in ["generation_mode", "required_gates", "typo"] {
        let mut manifest = valid_manifest("workflow.api.json");
        manifest[field] = json!("invalid");
        let path = create_workflow(&temp, manifest, valid_workflow());
        assert!(LoadedWorkflow::load(&path)
            .unwrap_err()
            .to_string()
            .contains("unknown field"));
    }
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["parameters"]["seed"]["typo"] = json!(true);
    let path = create_workflow(&temp, manifest, valid_workflow());
    assert!(LoadedWorkflow::load(&path)
        .unwrap_err()
        .to_string()
        .contains("unknown field"));
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["schema_version"] = json!(3);
    let path = create_workflow(&temp, manifest, valid_workflow());
    assert!(LoadedWorkflow::load(&path)
        .unwrap_err()
        .to_string()
        .contains("schema version 3, expected 2"));
}

#[test]
fn v2_binding_validation_preserves_existing_constraints() {
    let temp = TempDir::new().expect("temp directory");
    let mut manifest = valid_manifest("workflow.api.json");
    manifest["parameters"]["steps"]["allowed_values"] = json!([5, 10, 100]);
    let path = create_workflow(&temp, manifest.clone(), valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid workflow");
    assert!(loaded
        .bind(Some("missing"), &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("not defined"));
    for (value, message) in [
        (json!("10"), "expects Integer"),
        (json!(6), "allowed set"),
        (json!(101), "allowed set"),
    ] {
        let error = loaded
            .bind(
                None,
                &BTreeMap::from([("seed".into(), json!(1)), ("steps".into(), value)]),
            )
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    assert!(loaded
        .bind(None, &BTreeMap::from([("unknown".into(), json!(1))]))
        .unwrap_err()
        .to_string()
        .contains("not defined"));
    let mut range_manifest = valid_manifest("workflow.api.json");
    let range_path = create_workflow(&temp, range_manifest.clone(), valid_workflow());
    let ranged = LoadedWorkflow::load(&range_path).expect("range workflow");
    for (value, message) in [(0, "below minimum"), (101, "above maximum")] {
        let error = ranged
            .bind(
                None,
                &BTreeMap::from([("seed".into(), json!(1)), ("steps".into(), json!(value))]),
            )
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    for value in [1, 100] {
        assert!(ranged
            .bind(
                None,
                &BTreeMap::from([("seed".into(), json!(1)), ("steps".into(), json!(value))])
            )
            .is_ok());
    }
    range_manifest["presets"]["fast"]["unknown"] = json!(1);
    let path = create_workflow(&temp, range_manifest, valid_workflow());
    assert!(LoadedWorkflow::load(&path)
        .unwrap_err()
        .to_string()
        .contains("unknown parameter"));
    manifest["parameters"]["steps"]["default"] = json!(101);
    let path = create_workflow(&temp, manifest.clone(), valid_workflow());
    assert!(LoadedWorkflow::load(&path)
        .unwrap_err()
        .to_string()
        .contains("allowed set"));
    manifest["parameters"]["steps"]
        .as_object_mut()
        .unwrap()
        .remove("default");
    manifest["presets"]["fast"]["steps"] = json!("wrong");
    let path = create_workflow(&temp, manifest, valid_workflow());
    assert!(LoadedWorkflow::load(&path)
        .unwrap_err()
        .to_string()
        .contains("expects Integer"));
}

#[test]
fn workflow_rejects_ui_export_with_actionable_error() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(
        &temp,
        valid_manifest("workflow.api.json"),
        json!({ "nodes": [], "links": [] }),
    );
    let error = LoadedWorkflow::load(&path).expect_err("UI export must fail");

    assert!(
        error.to_string().contains("API format"),
        "unexpected error: {error}"
    );
}

#[test]
fn workflow_rejects_out_of_range_parameter_before_submission() {
    let temp = TempDir::new().expect("temp directory");
    let path = create_workflow(&temp, valid_manifest("workflow.api.json"), valid_workflow());
    let loaded = LoadedWorkflow::load(&path).expect("valid workflow");
    let error = loaded
        .bind(
            None,
            &BTreeMap::from([
                ("seed".to_owned(), json!(1)),
                ("steps".to_owned(), json!(101)),
            ]),
        )
        .expect_err("range must fail");

    assert!(error.to_string().contains("above maximum"));
}

#[test]
fn legacy_generic_run_and_manifest_are_still_readable() {
    let temp = TempDir::new().expect("temp directory");
    let path = temp.path().join("state.sqlite3");
    let database = rusqlite::Connection::open(&path).expect("legacy database");
    database
        .execute_batch(include_str!("fixtures/legacy-runs.sql"))
        .expect("install legacy schema and run");
    drop(database);

    let repository = RunRepository::open(&path).expect("open legacy database");
    let run = repository.get("run-legacy").expect("read legacy run");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.generation_mode, GenerationMode::Generic);
    assert!(run.updated_at > run.created_at);

    let manifest: RunManifest =
        serde_json::from_str(include_str!("fixtures/legacy-run-manifest.json"))
            .expect("read legacy manifest");
    assert_eq!(manifest.run_id, run.id);
    assert_eq!(manifest.prompt_id, run.prompt_id);
    assert_eq!(manifest.workflow.id, run.workflow_id);
    assert_eq!(manifest.parameters, run.parameters);
    assert_eq!(manifest.created_at, run.created_at);
    assert_eq!(Some(manifest.completed_at), run.completed_at);
    assert_eq!(manifest.outputs[0].source_filename, "example.png");

    repository
        .insert(&NewRun {
            id: "run-new".to_owned(),
            prompt_id: "prompt-new".to_owned(),
            client_id: "client-new".to_owned(),
            workflow_id: run.workflow_id,
            workflow_version: run.workflow_version,
            workflow_sha256: run.workflow_sha256,
            generation_mode: run.generation_mode,
            status: RunStatus::Queued,
            parameters: run.parameters,
            output_dir: "runs/text-to-image/v2".to_owned(),
        })
        .expect("insert new run into legacy database");
    assert_eq!(repository.list(10).expect("list old and new runs").len(), 2);
}
