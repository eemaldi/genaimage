use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use genaimage_core::{
    checkpoint_profiles::parse_profiles,
    workflow::{LoadedWorkflow, PresetKind},
};
use serde_json::{json, Value};

const WORKFLOWS: [&str; 4] = ["portrait", "sketch-to-image", "inpaint", "turnaround"];
const SDXL: &str = "sd_xl_base_1.0.safetensors";
const SD15: &str = "v1-5-pruned-emaonly.safetensors";

fn pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workflows/packs/character-art")
}

fn load(id: &str) -> LoadedWorkflow {
    LoadedWorkflow::load(&pack_dir().join(id).join("workflow.genaimage.json"))
        .unwrap_or_else(|error| panic!("{id} must load: {error}"))
}

fn overrides(value: Value) -> BTreeMap<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn profiles() -> Vec<genaimage_core::checkpoint_profiles::CheckpointProfile> {
    let source = fs::read_to_string(pack_dir().join("checkpoints.toml")).unwrap();
    parse_profiles(&source).expect("the pack's profiles must parse")
}

#[test]
fn portrait_defaults_bind_to_their_node_inputs_with_exact_numbers() {
    let bound = load("portrait")
        .bind(
            None,
            &overrides(json!({"checkpoint": SDXL, "prompt": "a knight in a field"})),
        )
        .unwrap();
    let graph = &bound.workflow;
    assert_eq!(graph["4"]["inputs"]["ckpt_name"], SDXL);
    assert_eq!(graph["6"]["inputs"]["text"], "a knight in a field");
    assert!(graph["7"]["inputs"]["text"]
        .as_str()
        .unwrap()
        .contains("glossy render"));
    assert_eq!(graph["3"]["inputs"]["seed"], 42);
    assert_eq!(graph["3"]["inputs"]["steps"], 28);
    assert_eq!(graph["3"]["inputs"]["cfg"].to_string(), "7.0");
    assert_eq!(graph["3"]["inputs"]["sampler_name"], "dpmpp_2m");
    assert_eq!(graph["3"]["inputs"]["scheduler"], "karras");
    assert_eq!(graph["3"]["inputs"]["denoise"].to_string(), "1.0");
    assert_eq!(graph["5"]["inputs"]["width"], 832);
    assert_eq!(graph["5"]["inputs"]["height"], 1248);
    assert_eq!(
        graph["9"]["inputs"]["filename_prefix"],
        "character-art-portrait"
    );
}

#[test]
fn every_layout_preset_sets_its_canvas_and_is_labelled_as_a_layout() {
    let expected = [
        ("portrait", "portrait-2x3", 832, 1248),
        ("portrait", "portrait-3x4", 896, 1184),
        ("portrait", "square", 1024, 1024),
        ("turnaround", "turnaround-3-view", 1536, 768),
    ];
    for (workflow_id, preset, width, height) in expected {
        let workflow = load(workflow_id);
        let info = &workflow.manifest.preset_info[preset];
        assert_eq!(info.kind, Some(PresetKind::Layout), "{preset}");
        assert!(info.label.as_deref().is_some_and(|label| !label.is_empty()));
        let bound = workflow
            .bind(
                Some(preset),
                &overrides(json!({"checkpoint": SDXL, "prompt": "a figure"})),
            )
            .unwrap();
        assert_eq!(bound.workflow["5"]["inputs"]["width"], width, "{preset}");
        assert_eq!(bound.workflow["5"]["inputs"]["height"], height, "{preset}");
        assert_eq!(width % 8, 0);
        assert_eq!(height % 8, 0);
    }
    let portrait = load("portrait");
    assert_eq!(
        portrait.manifest.preset_info["portrait-2x3"]
            .label
            .as_deref(),
        Some("Portrait 2:3")
    );
}

#[test]
fn the_turnaround_workflow_uses_the_wide_canvas_by_default() {
    let bound = load("turnaround")
        .bind(
            None,
            &overrides(json!({"checkpoint": SDXL, "prompt": "a turnaround"})),
        )
        .unwrap();
    assert_eq!(bound.workflow["5"]["inputs"]["width"], 1536);
    assert_eq!(bound.workflow["5"]["inputs"]["height"], 768);
    assert!(bound.workflow["7"]["inputs"]["text"]
        .as_str()
        .unwrap()
        .contains("overlapping figures"));
}

#[test]
fn sketch_and_inpaint_bind_their_uploads_to_load_image_nodes() {
    let sketch = load("sketch-to-image");
    let bound = sketch
        .bind(
            None,
            &overrides(json!({
                "checkpoint": SDXL,
                "sketch": "sha256:aa",
                "prompt": "painted over the sketch"
            })),
        )
        .unwrap();
    assert_eq!(bound.workflow["3"]["inputs"]["denoise"].to_string(), "0.6");
    assert_eq!(bound.workflow["11"]["inputs"]["pixels"], json!(["10", 0]));
    assert_eq!(bound.image_uploads["sketch"], PathBuf::from("sha256:aa"));
    let mut bound = bound;
    sketch
        .apply_uploaded_images(
            &mut bound,
            &BTreeMap::from([("sketch".to_owned(), "sketch.png".to_owned())]),
        )
        .unwrap();
    assert_eq!(bound.workflow["10"]["inputs"]["image"], "sketch.png");

    let inpaint = load("inpaint");
    let mut bound = inpaint
        .bind(
            None,
            &overrides(json!({
                "checkpoint": SDXL,
                "source_image": "sha256:bb",
                "mask_image": "sha256:cc",
                "prompt": "a new hat"
            })),
        )
        .unwrap();
    assert_eq!(bound.workflow["3"]["inputs"]["denoise"].to_string(), "1.0");
    assert_eq!(bound.workflow["11"]["inputs"]["grow_mask_by"], 6);
    assert_eq!(bound.workflow["11"]["inputs"]["mask"], json!(["13", 0]));
    assert_eq!(bound.workflow["13"]["inputs"]["channel"], "red");
    inpaint
        .apply_uploaded_images(
            &mut bound,
            &BTreeMap::from([
                ("source_image".to_owned(), "source.png".to_owned()),
                ("mask_image".to_owned(), "mask.png".to_owned()),
            ]),
        )
        .unwrap();
    assert_eq!(bound.workflow["10"]["inputs"]["image"], "source.png");
    assert_eq!(bound.workflow["12"]["inputs"]["image"], "mask.png");
}

#[test]
fn image_inputs_and_the_checkpoint_are_never_defaulted() {
    for id in WORKFLOWS {
        let workflow = load(id);
        let checkpoint = &workflow.manifest.parameters["checkpoint"];
        assert!(checkpoint.required && checkpoint.default.is_none(), "{id}");
        for (name, definition) in &workflow.manifest.parameters {
            if definition.kind == genaimage_core::ParameterKind::ImageUpload {
                assert!(definition.default.is_none(), "{id}.{name} has a default");
                assert!(definition.required, "{id}.{name} must be required");
            }
        }
    }
    let error = load("portrait")
        .bind(None, &overrides(json!({"prompt": "no checkpoint"})))
        .unwrap_err();
    assert!(error.to_string().contains("checkpoint"), "{error}");
    let error = load("sketch-to-image")
        .bind(
            None,
            &overrides(json!({"checkpoint": SDXL, "prompt": "no sketch"})),
        )
        .unwrap_err();
    assert!(error.to_string().contains("sketch"), "{error}");
}

#[test]
fn profiles_apply_to_every_workflow_with_no_ignored_values_and_no_forbidden_keys() {
    let profiles = profiles();
    assert_eq!(
        profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["sdxl-base", "sd15"]
    );
    for profile in &profiles {
        for id in WORKFLOWS {
            let workflow = load(id);
            assert!(profile.ignored_for(&workflow.manifest).is_empty(), "{id}");
            let applied: Vec<String> = profile.values_for(&workflow.manifest).into_keys().collect();
            assert_eq!(
                applied,
                ["cfg", "sampler_name", "scheduler", "steps"],
                "{id}"
            );
            assert!(!profile.values.contains_key("seed"));
            assert!(!profile.values.contains_key("checkpoint"));
        }
        assert!(profile.revision().starts_with("sha256:"));
    }
}

#[test]
fn profile_values_keep_their_exact_numbers_when_bound() {
    let profiles = profiles();
    let sdxl = profiles.iter().find(|p| p.id == "sdxl-base").unwrap();
    let sd15 = profiles.iter().find(|p| p.id == "sd15").unwrap();
    assert!(sdxl.matches(SDXL) && !sdxl.matches(SD15));
    assert!(sd15.matches(SD15));
    for (profile, checkpoint, steps, cfg) in [(sdxl, SDXL, 30, "7.0"), (sd15, SD15, 25, "7.5")] {
        let workflow = load("turnaround");
        let mut values = profile.values_for(&workflow.manifest);
        values.insert("checkpoint".to_owned(), json!(checkpoint));
        values.insert("prompt".to_owned(), json!("a turnaround"));
        let bound = workflow.bind(None, &values).unwrap();
        assert_eq!(bound.workflow["3"]["inputs"]["steps"], steps);
        assert_eq!(bound.workflow["3"]["inputs"]["cfg"].to_string(), cfg);
        assert_eq!(bound.workflow["4"]["inputs"]["ckpt_name"], checkpoint);
    }
    assert!(sd15.note.as_deref().unwrap().contains("512 pixels"));
}

#[test]
fn the_pack_declares_every_node_class_its_graphs_use() {
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(pack_dir().join("pack.toml")).unwrap()).unwrap();
    let declared: BTreeSet<String> = manifest["requires"]["node_classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|class| class.as_str().unwrap().to_owned())
        .collect();
    let mut used = BTreeSet::new();
    for id in WORKFLOWS {
        let graph: Value = serde_json::from_slice(
            &fs::read(pack_dir().join(id).join("api-workflow.json")).unwrap(),
        )
        .unwrap();
        for node in graph.as_object().unwrap().values() {
            used.insert(node["class_type"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(declared, used);
}

#[test]
fn the_concept_art_templates_keep_the_required_turnaround_wording() {
    let turnaround = fs::read_to_string(pack_dir().join("templates/turnaround.toml")).unwrap();
    assert!(turnaround.contains(
        "character turnaround sheet, front view, side view, back view, same character, consistent design, neutral pose, plain background"
    ));
    let concept = fs::read_to_string(pack_dir().join("templates/concept-art.toml")).unwrap();
    for placeholder in [
        "{{subject}}",
        "{{mandatory}}",
        "{{flexible}}",
        "{{palette}}",
        "{{view}}",
    ] {
        assert!(concept.contains(placeholder), "{placeholder}");
    }
}
