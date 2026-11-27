use serde_json::json;

use super::*;

const TEXT_TO_IMAGE: &str =
    include_str!("../../../../workflows/packs/core/text-to-image/api-workflow.json");
const IMG2IMG: &str = include_str!("../../tests/fixtures/import/img2img.json");
const LORA: &str = include_str!("../../tests/fixtures/import/lora.json");
const CUSTOM: &str = include_str!("../../tests/fixtures/import/custom-node.json");
const UI_FORMAT: &str = include_str!("../../tests/fixtures/import/ui-format.json");

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// Writes the proposal the way confirmation does and loads it with the
/// discovery loader, which is the check that matters.
fn assert_loads(proposal: &ImportProposal, graph: &Value) -> LoadedWorkflow {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join(GRAPH_FILE),
        canonical_graph_bytes(graph),
    )
    .unwrap();
    fs::write(
        directory.path().join(MANIFEST_FILE),
        manifest_bytes(&proposal.manifest),
    )
    .unwrap();
    let loaded = LoadedWorkflow::load(&directory.path().join(MANIFEST_FILE)).unwrap();
    assert_eq!(loaded.workflow_sha256, proposal.graph_sha256);
    loaded
}

fn parameter<'a>(proposal: &'a ImportProposal, name: &str) -> &'a ParameterDefinition {
    proposal.manifest.parameters.get(name).unwrap_or_else(|| {
        panic!(
            "no parameter '{name}' in {:?}",
            proposal.manifest.parameters.keys()
        )
    })
}

fn default_text(proposal: &ImportProposal, name: &str) -> String {
    parameter(proposal, name)
        .default
        .as_ref()
        .unwrap()
        .to_string()
}

#[test]
fn the_bundled_text_to_image_graph_gets_a_valid_manifest() {
    let graph = parse(TEXT_TO_IMAGE);
    let proposal = propose(&graph, None).unwrap();
    assert_loads(&proposal, &graph);
    let names: Vec<_> = proposal.manifest.parameters.keys().cloned().collect();
    assert_eq!(
        names,
        [
            "batch_size",
            "cfg",
            "checkpoint",
            "denoise",
            "height",
            "negative_prompt",
            "prompt",
            "sampler_name",
            "scheduler",
            "seed",
            "steps",
            "width"
        ]
    );
    assert_eq!(parameter(&proposal, "prompt").node, "6");
    assert_eq!(parameter(&proposal, "negative_prompt").node, "7");
    assert_eq!(parameter(&proposal, "checkpoint").node, "4");
    assert_eq!(parameter(&proposal, "seed").kind, ParameterKind::Seed);
    assert_eq!(default_text(&proposal, "seed"), "42");
    assert_eq!(
        default_text(&proposal, "prompt"),
        "\"replace with an explicit approved prompt\""
    );
    assert_eq!(
        parameter(&proposal, "steps")
            .minimum
            .as_ref()
            .unwrap()
            .to_string(),
        "1"
    );
    assert_eq!(proposal.manifest.schema_version, 2);
    assert_eq!(proposal.manifest.version, "1");
    assert_eq!(proposal.manifest.id, "imported-workflow");
    assert!(proposal.manifest.presets.is_empty());
    assert_eq!(proposal.manifest.outputs.len(), 1);
    assert_eq!(proposal.manifest.outputs[0].node, "9");
    let unknown: Vec<_> = proposal
        .unknown_nodes
        .iter()
        .map(|node| (node.node.as_str(), node.class_type.as_str()))
        .collect();
    assert_eq!(unknown, [("8", "VAEDecode")]);
}

#[test]
fn img2img_binds_an_upload_without_using_the_comfyui_side_file_as_default() {
    let graph = parse(IMG2IMG);
    let proposal = propose(&graph, None).unwrap();
    assert_loads(&proposal, &graph);
    let image = parameter(&proposal, "image");
    assert_eq!(image.kind, ParameterKind::ImageUpload);
    assert!(image.required);
    assert!(image.default.is_none());
    assert_eq!(image.widget.as_deref(), Some("file"));
    assert!(proposal
        .warnings
        .iter()
        .any(|warning| warning.contains("LoadImage node '11'")));
    // Numbers stay exact: a seed above 2^53 and a decimal point on a whole number.
    assert_eq!(default_text(&proposal, "seed"), "18446744073709551615");
    assert_eq!(default_text(&proposal, "cfg"), "2.0");
    assert_eq!(default_text(&proposal, "denoise"), "0.75");
    assert_eq!(parameter(&proposal, "prompt").node, "13");
    assert_eq!(parameter(&proposal, "negative_prompt").node, "14");
    assert!(proposal
        .unknown_nodes
        .iter()
        .any(|node| node.class_type == "VAEEncode"));
}

#[test]
fn lora_loaders_get_unique_names_and_exact_strengths() {
    let graph = parse(LORA);
    let proposal = propose(&graph, None).unwrap();
    assert_loads(&proposal, &graph);
    assert_eq!(parameter(&proposal, "lora_name_2").node, "2");
    assert_eq!(parameter(&proposal, "lora_name_3").node, "3");
    assert!(!proposal.manifest.parameters.contains_key("lora_name"));
    assert_eq!(default_text(&proposal, "lora_strength_model_2"), "-0.0");
    assert_eq!(default_text(&proposal, "lora_strength_model_3"), "1.0");
    // Only one loader has a CLIP strength, so it keeps the plain name.
    assert_eq!(parameter(&proposal, "lora_strength_clip").node, "2");
    assert_eq!(parameter(&proposal, "batch_size").node, "6");
    assert_eq!(default_text(&proposal, "negative_prompt"), "\"\"");
}

#[test]
fn unknown_custom_nodes_are_listed_and_never_bound() {
    let graph = parse(CUSTOM);
    let proposal = propose(&graph, None).unwrap();
    assert_loads(&proposal, &graph);
    assert!(proposal
        .unknown_nodes
        .iter()
        .any(|node| node.node == "7" && node.class_type == "VendorDetailBoost"));
    assert!(proposal
        .manifest
        .parameters
        .values()
        .all(|parameter| parameter.node != "7"));
    assert!(proposal.recognised.iter().all(|node| node.node != "7"));
}

#[test]
fn prompts_are_named_from_sampler_links_not_node_ids() {
    let mut graph = parse(TEXT_TO_IMAGE);
    graph["3"]["inputs"]["positive"] = json!(["7", 0]);
    graph["3"]["inputs"]["negative"] = json!(["6", 0]);
    let proposal = propose(&graph, None).unwrap();
    assert_eq!(parameter(&proposal, "prompt").node, "7");
    assert_eq!(parameter(&proposal, "negative_prompt").node, "6");
    assert_loads(&proposal, &graph);
}

#[test]
fn ambiguous_prompts_get_neutral_names_and_a_warning() {
    let mut graph = parse(TEXT_TO_IMAGE);
    graph["3"]["inputs"]["positive"] = json!(["8", 0]);
    let proposal = propose(&graph, None).unwrap();
    assert!(!proposal.manifest.parameters.contains_key("prompt"));
    assert_eq!(parameter(&proposal, "text_6").node, "6");
    assert_eq!(parameter(&proposal, "negative_prompt").node, "7");
    assert!(proposal
        .warnings
        .iter()
        .any(|warning| warning.contains("node '6'")));
    assert_loads(&proposal, &graph);
}

#[test]
fn links_and_unacceptable_literals_are_reported_not_bound() {
    let mut graph = parse(TEXT_TO_IMAGE);
    graph["3"]["inputs"]["steps"] = json!(0);
    graph["3"]["inputs"]["seed"] = parse("20.0");
    graph["3"]["inputs"]["denoise"] = json!(2);
    let proposal = propose(&graph, None).unwrap();
    for name in ["steps", "seed", "denoise"] {
        assert!(
            !proposal.manifest.parameters.contains_key(name),
            "{name} must not be bound"
        );
        let unbound = proposal
            .unbound_inputs
            .iter()
            .find(|item| item.node == "3" && item.input == name)
            .unwrap_or_else(|| panic!("{name} not reported"));
        assert!(!unbound.linked);
        assert!(
            unbound.reason.contains("not accepted"),
            "{}",
            unbound.reason
        );
    }
    let model = proposal
        .unbound_inputs
        .iter()
        .find(|item| item.node == "3" && item.input == "model")
        .unwrap();
    assert!(model.linked);
    assert!(model.reason.contains("node '4'"));
    assert!(proposal.unbound_inputs.iter().all(|item| item.node != "8"));
    assert_loads(&proposal, &graph);
}

#[test]
fn colliding_names_are_suffixed_deterministically() {
    let mut graph = parse(TEXT_TO_IMAGE);
    graph["20"] = json!({"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "other.safetensors"}});
    let proposal = propose(&graph, None).unwrap();
    assert_eq!(parameter(&proposal, "checkpoint_20").node, "20");
    assert_eq!(parameter(&proposal, "checkpoint_4").node, "4");
    assert!(!proposal.manifest.parameters.contains_key("checkpoint"));
    assert_loads(&proposal, &graph);
}

#[test]
fn proposals_do_not_depend_on_key_order() {
    let forward = parse(IMG2IMG);
    let mut reversed = Map::new();
    for (key, value) in forward.as_object().unwrap().iter().rev() {
        reversed.insert(key.clone(), value.clone());
    }
    let first = propose(&forward, Some("Fox")).unwrap();
    let second = propose(&Value::Object(reversed), Some("Fox")).unwrap();
    assert_eq!(first.graph_sha256, second.graph_sha256);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[test]
fn canonical_bytes_keep_exact_numbers() {
    let text = String::from_utf8(canonical_graph_bytes(&parse(LORA))).unwrap();
    assert!(text.contains("\"strength_model\":-0.0"));
    assert!(text.contains("\"strength_model\":1.0"));
    let text = String::from_utf8(canonical_graph_bytes(&parse(IMG2IMG))).unwrap();
    assert!(text.contains("18446744073709551615"));
    assert!(text.contains("\"cfg\":2.0"));
    assert!(text.ends_with('\n'));
}

#[test]
fn the_name_chooses_the_title_and_a_safe_id() {
    let graph = parse(TEXT_TO_IMAGE);
    let proposal = propose(&graph, Some("  My Cool Flow! ")).unwrap();
    assert_eq!(proposal.manifest.title, "My Cool Flow!");
    assert_eq!(proposal.manifest.id, "my-cool-flow");
    assert_eq!(
        propose(&graph, Some("import")).unwrap().manifest.id,
        "import-workflow"
    );
    assert_eq!(
        propose(&graph, Some("../../etc")).unwrap().manifest.id,
        "etc"
    );
    assert_eq!(
        propose(&graph, Some("   ")).unwrap().manifest.id,
        "imported-workflow"
    );
    assert!(is_safe_workflow_id(
        &propose(&graph, Some(&"a".repeat(500))).unwrap().manifest.id
    ));
}

#[test]
fn a_graph_without_save_image_is_proposed_with_a_warning() {
    let graph =
        json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "a"}}});
    let proposal = propose(&graph, None).unwrap();
    assert!(proposal.manifest.outputs.is_empty());
    assert!(proposal
        .warnings
        .iter()
        .any(|warning| warning.contains("SaveImage")));
}

#[test]
fn ui_exports_are_detected() {
    assert!(matches!(
        propose(&parse(UI_FORMAT), None),
        Err(ImportError::UiFormat)
    ));
    assert!(matches!(
        propose(&json!({"links": []}), None),
        Err(ImportError::UiFormat)
    ));
}

fn invalid(graph: &Value) -> Vec<ImportIssue> {
    match propose(graph, None) {
        Err(ImportError::InvalidGraph(issues)) => issues,
        other => panic!("expected InvalidGraph, got {other:?}"),
    }
}

#[test]
fn malformed_graphs_get_actionable_issues() {
    assert_eq!(invalid(&json!([1, 2]))[0].field, "graph");
    assert!(invalid(&json!({}))[0].reason.contains("no nodes"));
    let issues = invalid(&json!({
        "1": {"inputs": {}},
        "2": {"class_type": "KSampler"},
        "3": "text",
        "4": {"class_type": "", "inputs": {}}
    }));
    let fields: Vec<_> = issues.iter().map(|item| item.field.as_str()).collect();
    assert_eq!(fields, ["graph.1", "graph.2", "graph.3", "graph.4"]);
}

#[test]
fn node_and_input_counts_are_bounded() {
    let nodes: Map<String, Value> = (0..=MAX_NODES)
        .map(|id| (id.to_string(), json!({"class_type": "Note", "inputs": {}})))
        .collect();
    assert!(invalid(&Value::Object(nodes))[0]
        .reason
        .contains("1001 nodes"));
    let inputs: Map<String, Value> = (0..=MAX_INPUTS_PER_NODE)
        .map(|index| (format!("input{index}"), json!(index)))
        .collect();
    let issues = invalid(&json!({"1": {"class_type": "Note", "inputs": inputs}}));
    assert!(issues[0].reason.contains("per node"));
    let many_inputs: Map<String, Value> = (0..MAX_INPUTS_PER_NODE)
        .map(|index| (format!("input{index}"), json!(index)))
        .collect();
    let nodes: Map<String, Value> = (0..=MAX_TOTAL_INPUTS / MAX_INPUTS_PER_NODE)
        .map(|id| {
            (
                id.to_string(),
                json!({"class_type": "Note", "inputs": many_inputs}),
            )
        })
        .collect();
    assert!(invalid(&Value::Object(nodes))[0]
        .reason
        .contains("import limit"));
}

#[test]
fn oversized_graphs_are_rejected() {
    let graph =
        json!({"1": {"class_type": "Note", "inputs": {"text": "x".repeat(MAX_GRAPH_BYTES)}}});
    assert!(matches!(
        propose(&graph, None),
        Err(ImportError::GraphTooLarge { .. })
    ));
}

#[test]
fn only_safe_ids_pass() {
    for id in ["a", "text-to-image", "flow_2", "0abc"] {
        assert!(is_safe_workflow_id(id), "{id}");
    }
    for id in [
        "",
        "-a",
        "_a",
        "A",
        "a/b",
        "a b",
        "..",
        "a.b",
        "é",
        &"a".repeat(65),
    ] {
        assert!(!is_safe_workflow_id(id), "{id}");
    }
}

#[test]
fn nodes_and_links_are_not_node_ids() {
    for id in ["nodes", "links"] {
        let issues = invalid(&json!({
            "1": {"class_type": "Note", "inputs": {}},
            id: {"class_type": "Note", "inputs": {}}
        }));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].field, format!("graph.{id}"));
        assert!(issues[0].reason.contains("API format"), "{issues:?}");
    }
}

#[test]
fn manifest_issue_lists_are_bounded() {
    let graph = parse(TEXT_TO_IMAGE);
    let canonical = canonical_graph_bytes(&graph);
    let mut manifest = serde_json::to_value(propose(&graph, None).unwrap().manifest).unwrap();
    let parameters = manifest["parameters"].as_object_mut().unwrap();
    let template = parameters["steps"].clone();
    for index in 0..(MAX_ISSUES + 7) {
        let mut broken = template.clone();
        broken["node"] = json!(format!("missing-{index}"));
        parameters.insert(format!("extra_{index:02}"), broken);
    }
    let Err(ImportError::InvalidManifest(issues)) = validate_manifest(&manifest, &canonical) else {
        panic!("expected InvalidManifest");
    };
    assert!(issues.len() > MAX_ISSUES);
    assert_eq!(issues.len(), MAX_ISSUES + 1, "{issues:?}");
    let last = issues.last().unwrap();
    assert_eq!(last.reason, format!("and {} more", 7));
}

#[test]
fn an_explicit_null_default_is_refused_not_dropped() {
    let graph = parse(TEXT_TO_IMAGE);
    let canonical = canonical_graph_bytes(&graph);
    let mut manifest = serde_json::to_value(propose(&graph, None).unwrap().manifest).unwrap();
    manifest["parameters"]["steps"]["default"] = Value::Null;
    let Err(ImportError::InvalidManifest(issues)) = validate_manifest(&manifest, &canonical) else {
        panic!("expected InvalidManifest");
    };
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].field, "parameters.steps");
    assert!(issues[0].reason.contains("null"), "{issues:?}");
    manifest["parameters"]["steps"]
        .as_object_mut()
        .unwrap()
        .remove("default");
    assert!(validate_manifest(&manifest, &canonical).is_ok());
}

#[test]
fn a_null_default_in_a_manifest_file_is_a_discovery_error() {
    let graph = parse(TEXT_TO_IMAGE);
    let mut manifest = serde_json::to_value(propose(&graph, None).unwrap().manifest).unwrap();
    manifest["parameters"]["steps"]["default"] = Value::Null;
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join(GRAPH_FILE),
        canonical_graph_bytes(&graph),
    )
    .unwrap();
    fs::write(
        directory.path().join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let error = LoadedWorkflow::load(&directory.path().join(MANIFEST_FILE))
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains("null"), "{error}");
}

#[test]
fn rollback_removes_only_what_this_call_created() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(path.join(GRAPH_FILE), b"existing graph").unwrap();
    fs::write(path.join("notes.txt"), b"existing notes").unwrap();
    let mut created = Vec::new();
    let error = publish_new(path, GRAPH_FILE, b"new graph", &mut created).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    publish_new(path, MANIFEST_FILE, b"new manifest", &mut created).unwrap();
    rollback(path, &created, false);
    assert_eq!(fs::read(path.join(GRAPH_FILE)).unwrap(), b"existing graph");
    assert_eq!(fs::read(path.join("notes.txt")).unwrap(), b"existing notes");
    assert!(!path.join(MANIFEST_FILE).exists());
    let mut names: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["api-workflow.json", "notes.txt"]);
    rollback(path, &[], true);
    assert!(path.exists(), "a non-empty directory is never removed");
}

#[test]
fn hard_link_failures_name_the_requirement_but_clashes_stay_plain() {
    let target = Path::new("/w/x/workflow.genaimage.json");
    let refused = link_error(io::Error::from(io::ErrorKind::Unsupported), target);
    assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
    assert!(refused.to_string().contains("hard links"), "{refused}");
    assert!(refused.to_string().contains("paths.workflows"), "{refused}");
    let clash = link_error(io::Error::from(io::ErrorKind::AlreadyExists), target);
    assert_eq!(clash.kind(), io::ErrorKind::AlreadyExists);
    assert!(!clash.to_string().contains("hard links"));
}

#[test]
fn only_this_schemes_temporaries_count_as_leftovers() {
    let id = Uuid::new_v4();
    assert!(is_stale_temporary(&format!(".{GRAPH_FILE}.{id}.tmp")));
    assert!(is_stale_temporary(&format!(".{MANIFEST_FILE}.{id}.tmp")));
    for name in [
        "notes.tmp",
        ".api-workflow.json.not-a-uuid.tmp",
        &format!("{GRAPH_FILE}.{id}.tmp"),
        &format!(".other.json.{id}.tmp"),
    ] {
        assert!(!is_stale_temporary(name), "{name}");
    }
}

struct NoBackend;

fn unused<T>() -> crate::Result<T> {
    Err(HarnessError::UnsupportedOperation(
        "not used by import tests".to_owned(),
    ))
}

#[crate::async_trait]
impl crate::ImageBackend for NoBackend {
    fn name(&self) -> &str {
        "none"
    }
    fn base_url(&self) -> &str {
        "fake://local"
    }
    async fn health(&self) -> crate::Result<crate::image_backend::BackendHealth> {
        unused()
    }
    async fn object_info(&self) -> crate::Result<Value> {
        unused()
    }
    async fn queue(&self) -> crate::Result<crate::image_backend::QueueSnapshot> {
        unused()
    }
    async fn submit(
        &self,
        _: &Value,
        _: &str,
        _: &str,
    ) -> crate::Result<crate::image_backend::PromptResponse> {
        unused()
    }
    async fn history(&self, _: &str) -> crate::Result<crate::image_backend::HistorySnapshot> {
        unused()
    }
    async fn delete_queued(&self, _: &str) -> crate::Result<Value> {
        unused()
    }
    async fn interrupt(&self, _: &str) -> crate::Result<Value> {
        unused()
    }
    async fn upload_image(&self, _: &Path) -> crate::Result<crate::image_backend::UploadResponse> {
        unused()
    }
    async fn download_output(
        &self,
        _: &crate::image_backend::OutputReference,
    ) -> crate::Result<Vec<u8>> {
        unused()
    }
    async fn monitor_events(&self, _: &str) -> crate::Result<crate::image_backend::EventMonitor> {
        unused()
    }
}

/// Call A has published the graph and is about to link its manifest; call B
/// completes the directory first. A's `AlreadyExists` must lose the race
/// without removing anything B depends on.
#[test]
fn a_fresh_import_that_loses_the_manifest_race_removes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let config =
        crate::HarnessConfig::load_with(
            directory.path(),
            None,
            directory.path(),
            |key| match key {
                "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME" => {
                    Some(directory.path().display().to_string())
                }
                _ => None,
            },
        )
        .unwrap();
    let harness = Harness::new(config, std::sync::Arc::new(NoBackend)).unwrap();
    let root = harness.config().workflows.clone();
    let graph = parse(TEXT_TO_IMAGE);
    let proposal = propose(&graph, None).unwrap();
    let manifest = serde_json::to_value(&proposal.manifest).unwrap();
    let canonical = canonical_graph_bytes(&graph);
    let (parsed, ours) = validate_manifest(&manifest, &canonical).unwrap();
    let workflow_directory = root.join(&parsed.id);

    let mut different = manifest.clone();
    different["title"] = json!("Someone else's title");
    let (_, theirs) = validate_manifest(&different, &canonical).unwrap();
    assert_ne!(ours, theirs);

    for (b_manifest, b_bytes, a_wins_replay) in
        [(&manifest, &ours, true), (&different, &theirs, false)]
    {
        let _ = fs::remove_dir_all(&workflow_directory);
        create_root(&root).unwrap();
        fs::create_dir(&workflow_directory).unwrap();
        let mut a_created = Vec::new();
        publish_new(&workflow_directory, GRAPH_FILE, &canonical, &mut a_created).unwrap();

        let b = harness
            .confirm_workflow_import(&graph, &proposal.graph_sha256, b_manifest)
            .unwrap();
        assert_eq!(b.id, parsed.id);

        let a = harness.publish_manifest(&root, &workflow_directory, &parsed.id, &ours);
        match (a_wins_replay, a) {
            (true, Ok(replayed)) => assert_eq!(replayed.revision, b.revision),
            (false, Err(ImportError::Exists { id, .. })) => assert_eq!(id, parsed.id),
            (_, other) => panic!("unexpected result for A: {other:?}"),
        }
        assert_eq!(
            fs::read(workflow_directory.join(GRAPH_FILE)).unwrap(),
            canonical
        );
        assert_eq!(
            fs::read(workflow_directory.join(MANIFEST_FILE)).unwrap(),
            *b_bytes
        );
        LoadedWorkflow::load(&workflow_directory.join(MANIFEST_FILE)).unwrap();
        assert_eq!(
            harness
                .imported(&parsed.id, &workflow_directory)
                .unwrap()
                .revision,
            b.revision
        );
    }
}
