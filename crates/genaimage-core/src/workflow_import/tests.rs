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
