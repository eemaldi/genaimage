//! Checks a pack's declared requirements against ComfyUI's `/object_info`.
//!
//! One domain function serves the validate endpoint, the CLI and the doctor, so
//! no surface can report a looser result. Nothing is installed or substituted.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{models::loader_options, pack::PackRequires, Harness, Result};

pub const OBJECT_INFO_DEADLINE: Duration = Duration::from_secs(5);

/// Model folder to the loader node input that lists its files.
const FOLDER_LOADERS: &[(&str, &str, &str)] = &[
    ("checkpoints", "CheckpointLoaderSimple", "ckpt_name"),
    ("loras", "LoraLoader", "lora_name"),
    ("vae", "VAELoader", "vae_name"),
    ("controlnet", "ControlNetLoader", "control_net_name"),
    ("upscale_models", "UpscaleModelLoader", "model_name"),
    ("clip", "CLIPLoader", "clip_name"),
    ("clip_vision", "CLIPVisionLoader", "clip_name"),
    ("unet", "UNETLoader", "unet_name"),
    ("diffusion_models", "UNETLoader", "unet_name"),
    ("style_models", "StyleModelLoader", "style_model_name"),
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Available,
    Missing,
    /// The backend could not answer, so presence is neither confirmed nor denied.
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackStatus {
    /// Every declared requirement is confirmed present (or none is declared)
    /// and the pack has no file or workflow problems.
    Ok,
    /// A requirement is confirmed missing, or a declared file or workflow is
    /// missing, rejected or duplicated.
    Missing,
    /// Nothing is missing, but at least one requirement could not be checked.
    Unknown,
    /// The backend could not be queried; nothing was confirmed.
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NodeClassRequirement {
    pub class: String,
    pub status: RequirementStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelRequirement {
    pub folder: String,
    pub name: String,
    pub status: RequirementStatus,
    /// The loader input used for the check, as `Class.input`.
    pub loader: Option<String>,
    /// Why the status is `unknown`.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PackValidation {
    pub pack_id: String,
    pub status: PackStatus,
    /// Why the status is `unavailable`.
    pub reason: Option<String>,
    pub node_classes: Vec<NodeClassRequirement>,
    pub models: Vec<ModelRequirement>,
    /// Problems with the pack's own files and workflows. Never empty for a
    /// pack whose status is `ok`.
    pub diagnostics: Vec<String>,
}

impl PackValidation {
    /// Adds pack file and workflow problems. A pack with any problem is not
    /// `ok`: declared files that are missing or rejected count as `missing`.
    pub fn with_problems(mut self, problems: Vec<String>) -> Self {
        if !problems.is_empty() && self.status == PackStatus::Ok {
            self.status = PackStatus::Missing;
        }
        self.diagnostics = problems;
        self
    }
}

fn comparable(name: &str) -> String {
    name.replace('\\', "/")
}

/// Compares `requires` with `object_info`, or with the reason it could not be fetched.
pub fn check_requirements(
    pack_id: &str,
    requires: &PackRequires,
    object_info: std::result::Result<&Value, &str>,
) -> PackValidation {
    let result = |status, reason, node_classes, models| PackValidation {
        pack_id: pack_id.to_owned(),
        status,
        reason,
        node_classes,
        models,
        diagnostics: Vec::new(),
    };
    if requires.is_empty() {
        return result(PackStatus::Ok, None, Vec::new(), Vec::new());
    }
    let unavailable_reason = match object_info {
        Err(reason) => Some(reason.to_owned()),
        Ok(Value::Object(classes)) if classes.is_empty() => {
            Some("ComfyUI /object_info lists no node classes".to_owned())
        }
        Ok(Value::Object(_)) => None,
        Ok(_) => Some("ComfyUI /object_info did not return a JSON object".to_owned()),
    };
    let unknown_models = |reason: &dyn Fn(&str) -> String| -> Vec<ModelRequirement> {
        requires
            .models
            .iter()
            .flat_map(|(folder, names)| {
                names.iter().map(move |name| ModelRequirement {
                    folder: folder.clone(),
                    name: name.clone(),
                    status: RequirementStatus::Unknown,
                    loader: None,
                    reason: Some(reason(folder)),
                })
            })
            .collect()
    };
    let info = match (object_info, unavailable_reason) {
        (Ok(info), None) => info,
        (_, reason) => {
            let reason = reason.unwrap_or_default();
            let node_classes = requires
                .node_classes
                .iter()
                .map(|class| NodeClassRequirement {
                    class: class.clone(),
                    status: RequirementStatus::Unknown,
                })
                .collect();
            let note = format!("ComfyUI could not be queried: {reason}");
            let mut models = unknown_models(&|_| note.clone());
            for model in &mut models {
                model.loader =
                    loader_for(&model.folder).map(|(class, input)| format!("{class}.{input}"));
            }
            return result(PackStatus::Unavailable, Some(reason), node_classes, models);
        }
    };
    let node_classes: Vec<NodeClassRequirement> = requires
        .node_classes
        .iter()
        .map(|class| NodeClassRequirement {
            class: class.clone(),
            status: if info.get(class).is_some() {
                RequirementStatus::Available
            } else {
                RequirementStatus::Missing
            },
        })
        .collect();
    let mut models = Vec::new();
    for (folder, names) in &requires.models {
        let Some((class, input)) = loader_for(folder) else {
            models.extend(names.iter().map(|name| ModelRequirement {
                folder: folder.clone(),
                name: name.clone(),
                status: RequirementStatus::Unknown,
                loader: None,
                reason: Some(format!(
                    "model folder '{folder}' has no known loader node, so it cannot be checked"
                )),
            }));
            continue;
        };
        let loader = Some(format!("{class}.{input}"));
        let options = loader_options(info, class, input);
        for name in names {
            let (status, reason) = match &options {
                Some(options) if options.iter().any(|o| comparable(o) == comparable(name)) => {
                    (RequirementStatus::Available, None)
                }
                Some(_) => (RequirementStatus::Missing, None),
                None => (
                    RequirementStatus::Unknown,
                    Some(format!(
                        "ComfyUI does not list the options of {class}.{input}, so '{folder}' cannot be checked"
                    )),
                ),
            };
            models.push(ModelRequirement {
                folder: folder.clone(),
                name: name.clone(),
                status,
                loader: loader.clone(),
                reason,
            });
        }
    }
    let statuses = || {
        node_classes
            .iter()
            .map(|item| item.status)
            .chain(models.iter().map(|item| item.status))
    };
    let status = if statuses().any(|s| s == RequirementStatus::Missing) {
        PackStatus::Missing
    } else if statuses().any(|s| s == RequirementStatus::Unknown) {
        PackStatus::Unknown
    } else {
        PackStatus::Ok
    };
    result(status, None, node_classes, models)
}

fn loader_for(folder: &str) -> Option<(&'static str, &'static str)> {
    FOLDER_LOADERS
        .iter()
        .find(|(name, ..)| *name == folder)
        .map(|(_, class, input)| (*class, *input))
}

impl Harness {
    /// Fetches `/object_info` once, bounded, for a requirement check. Errors are reasons, not results.
    pub async fn fetch_object_info(&self) -> std::result::Result<Value, String> {
        match tokio::time::timeout(OBJECT_INFO_DEADLINE, self.backend().object_info()).await {
            Ok(Ok(info)) => Ok(info),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!(
                "ComfyUI /object_info did not answer within {} s",
                OBJECT_INFO_DEADLINE.as_secs()
            )),
        }
    }

    /// Validates one discovered pack against the backend. `Ok(None)` when no
    /// valid pack has that id. Never installs anything.
    pub async fn validate_pack(&self, id: &str) -> Result<Option<PackValidation>> {
        let harness = self.clone();
        let discovery = tokio::task::spawn_blocking(move || harness.discover_workflows())
            .await
            .map_err(|error| {
                crate::HarnessError::Pack(format!("pack discovery failed: {error}"))
            })??;
        let Some(pack) = discovery.packs.pack(id) else {
            return Ok(None);
        };
        let requires = &pack.manifest.requires;
        let info = if requires.is_empty() {
            Err(String::new())
        } else {
            self.fetch_object_info().await
        };
        Ok(Some(
            check_requirements(id, requires, info.as_ref().map_err(String::as_str))
                .with_problems(discovery.pack_problems(pack)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn info() -> Value {
        json!({
            "KSampler": {},
            "CheckpointLoaderSimple": {"input": {"required": {
                "ckpt_name": [["base.safetensors", "sub\\other.safetensors"]]
            }}},
            "LoraLoader": {"input": {"required": {
                "lora_name": ["COMBO", {"options": ["style.safetensors"]}]
            }}}
        })
    }

    fn requires(source: &str) -> PackRequires {
        crate::pack::PackManifest::parse(&format!("id='p'\nversion='1'\n{source}"))
            .unwrap()
            .requires
    }

    fn check(source: &str, info: std::result::Result<&Value, &str>) -> PackValidation {
        check_requirements("p", &requires(source), info)
    }

    #[test]
    fn every_present_requirement_is_ok_in_both_options_shapes() {
        let info = info();
        let result = check(
            "[requires]\nnode_classes=['KSampler']\n[requires.models]\ncheckpoints=['base.safetensors','sub/other.safetensors']\nloras=['style.safetensors']\n",
            Ok(&info),
        );
        assert_eq!(result.status, PackStatus::Ok);
        assert!(result
            .models
            .iter()
            .all(|m| m.status == RequirementStatus::Available));
        assert_eq!(
            result.models[2].loader.as_deref(),
            Some("LoraLoader.lora_name")
        );
    }

    #[test]
    fn reports_missing_nodes_and_models_individually() {
        let info = info();
        let result = check(
            "[requires]\nnode_classes=['KSampler','Absent']\n[requires.models]\ncheckpoints=['base.safetensors','gone.safetensors']\nloras=['style.safetensors']\n",
            Ok(&info),
        );
        assert_eq!(result.status, PackStatus::Missing);
        let nodes: Vec<_> = result
            .node_classes
            .iter()
            .map(|n| (n.class.as_str(), n.status))
            .collect();
        assert_eq!(
            nodes,
            [
                ("KSampler", RequirementStatus::Available),
                ("Absent", RequirementStatus::Missing)
            ]
        );
        let models: Vec<_> = result
            .models
            .iter()
            .map(|m| (m.name.as_str(), m.status))
            .collect();
        assert_eq!(
            models,
            [
                ("base.safetensors", RequirementStatus::Available),
                ("gone.safetensors", RequirementStatus::Missing),
                ("style.safetensors", RequirementStatus::Available),
            ]
        );
    }

    #[test]
    fn an_unknown_folder_is_unknown_never_ok() {
        let info = info();
        let result = check("[requires.models]\nembeddings=['e.pt']\n", Ok(&info));
        assert_eq!(result.status, PackStatus::Unknown);
        assert_eq!(result.models[0].status, RequirementStatus::Unknown);
        assert!(result.models[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("no known loader"));
    }

    #[test]
    fn a_missing_loader_node_leaves_models_unknown_not_missing() {
        let info = json!({"KSampler": {}});
        let result = check("[requires.models]\nloras=['x']\n", Ok(&info));
        assert_eq!(result.status, PackStatus::Unknown);
    }

    #[test]
    fn missing_outranks_unknown() {
        let info = info();
        let result = check(
            "[requires]\nnode_classes=['Absent']\n[requires.models]\nembeddings=['e']\n",
            Ok(&info),
        );
        assert_eq!(result.status, PackStatus::Missing);
    }

    #[test]
    fn an_unreachable_backend_confirms_nothing() {
        let result = check(
            "[requires]\nnode_classes=['KSampler']\n[requires.models]\nloras=['x']\n",
            Err("connection refused"),
        );
        assert_eq!(result.status, PackStatus::Unavailable);
        assert_eq!(result.reason.as_deref(), Some("connection refused"));
        assert_eq!(result.node_classes[0].status, RequirementStatus::Unknown);
        assert_eq!(result.models[0].status, RequirementStatus::Unknown);
        for odd in [json!([]), json!({}), json!("x")] {
            assert_eq!(
                check("[requires]\nnode_classes=['KSampler']\n", Ok(&odd)).status,
                PackStatus::Unavailable
            );
        }
    }

    #[test]
    fn a_pack_without_requirements_is_ok_even_offline() {
        assert_eq!(check("", Err("down")).status, PackStatus::Ok);
    }
}
