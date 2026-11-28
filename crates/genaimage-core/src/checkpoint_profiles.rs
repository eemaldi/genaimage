//! Checkpoint profiles: parameter values a pack declares for exact checkpoint
//! names. A profile is applied only when a run request names it, and only when
//! the request's checkpoint is one of the profile's exact names. No model
//! family is ever inferred from a file name.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    orchestrator::WorkflowSource,
    pack::{validate_pack_id, LoadedPack},
    workflow::{LoadedWorkflow, WorkflowManifest},
    Harness, HarnessError, Result,
};

pub const MAX_PROFILES: usize = 64;
pub const MAX_PROFILE_CHECKPOINTS: usize = 64;
pub const MAX_PROFILE_VALUES: usize = 64;
const MAX_TEXT_BYTES: usize = 256;
const MAX_NOTE_BYTES: usize = 1000;
const MAX_STRING_VALUE_BYTES: usize = 2000;

/// One profile of a pack's `checkpoints.toml`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointProfile {
    pub id: String,
    pub label: String,
    /// Exact checkpoint names; matching is string equality.
    pub checkpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Parameter name to value, applied to every pack workflow that defines it.
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<CheckpointProfile>,
}

fn one_line(kind: &str, value: &str, max: usize) -> std::result::Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.contains(['\0', '\n']) {
        return Err(format!(
            "{kind} '{}' must be 1 to {max} bytes on one line",
            value.escape_debug()
        ));
    }
    Ok(())
}

impl CheckpointProfile {
    fn validate(&self) -> std::result::Result<(), String> {
        let id = &self.id;
        validate_pack_id(id).map_err(|error| format!("profile {error}"))?;
        one_line("label", &self.label, MAX_TEXT_BYTES)
            .map_err(|e| format!("profile '{id}': {e}"))?;
        if let Some(note) = &self.note {
            if note.trim().is_empty() || note.len() > MAX_NOTE_BYTES || note.contains('\0') {
                return Err(format!(
                    "profile '{id}': note must be 1 to {MAX_NOTE_BYTES} bytes"
                ));
            }
        }
        if self.checkpoints.is_empty() || self.checkpoints.len() > MAX_PROFILE_CHECKPOINTS {
            return Err(format!(
                "profile '{id}': list 1 to {MAX_PROFILE_CHECKPOINTS} exact checkpoint names"
            ));
        }
        let mut seen = BTreeSet::new();
        for name in &self.checkpoints {
            one_line("checkpoint name", name, MAX_TEXT_BYTES)
                .map_err(|e| format!("profile '{id}': {e}"))?;
            if !seen.insert(name) {
                return Err(format!(
                    "profile '{id}': checkpoint '{name}' is listed twice"
                ));
            }
        }
        if self.values.len() > MAX_PROFILE_VALUES {
            return Err(format!(
                "profile '{id}': at most {MAX_PROFILE_VALUES} values"
            ));
        }
        for (name, value) in &self.values {
            one_line("parameter name", name, MAX_TEXT_BYTES)
                .map_err(|e| format!("profile '{id}': {e}"))?;
            let scalar = match value {
                Value::String(text) => text.len() <= MAX_STRING_VALUE_BYTES,
                Value::Number(_) | Value::Bool(_) => true,
                _ => false,
            };
            if !scalar {
                return Err(format!(
                    "profile '{id}': value of '{name}' must be a string of at most {MAX_STRING_VALUE_BYTES} bytes, a finite number or a boolean"
                ));
            }
        }
        Ok(())
    }

    pub fn matches(&self, checkpoint: &str) -> bool {
        self.checkpoints.iter().any(|name| name == checkpoint)
    }

    /// The values that apply to one workflow: those naming a parameter it defines.
    pub fn values_for(&self, manifest: &WorkflowManifest) -> BTreeMap<String, Value> {
        self.values
            .iter()
            .filter(|(name, _)| manifest.parameters.contains_key(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// Parses and validates the text of a `checkpoints.toml`.
pub fn parse_profiles(source: &str) -> std::result::Result<Vec<CheckpointProfile>, String> {
    let file: ProfilesFile = toml::from_str(source).map_err(|error| error.to_string())?;
    if file.profiles.len() > MAX_PROFILES {
        return Err(format!("at most {MAX_PROFILES} profiles"));
    }
    let mut ids = BTreeSet::new();
    for profile in &file.profiles {
        profile.validate()?;
        if !ids.insert(&profile.id) {
            return Err(format!("profile '{}' is declared twice", profile.id));
        }
    }
    Ok(file.profiles)
}

/// Problems that need the pack's workflows: values the workflows would refuse
/// and parameter names that no workflow of the pack defines.
pub(crate) fn value_problems(
    pack: &LoadedPack,
    workflows: &[crate::orchestrator::DiscoveredWorkflow],
) -> Vec<String> {
    let id = &pack.manifest.id;
    let members: Vec<_> = workflows
        .iter()
        .filter(|w| matches!(&w.source, WorkflowSource::Pack(source) if source == id))
        .collect();
    let mut problems = Vec::new();
    for profile in &pack.checkpoint_profiles {
        for (name, value) in &profile.values {
            let mut defined = false;
            for workflow in &members {
                if let Some(definition) = workflow.manifest.parameters.get(name) {
                    defined = true;
                    if let Err(error) = definition.validate_value(name, value) {
                        problems.push(format!(
                            "checkpoint profile '{}' is invalid for workflow '{}': {error}",
                            profile.id, workflow.manifest.id
                        ));
                    }
                }
            }
            if !defined && !members.is_empty() {
                problems.push(format!(
                    "checkpoint profile '{}' sets '{name}', which no workflow of pack '{id}' defines",
                    profile.id
                ));
            }
        }
    }
    problems
}

/// A profile chosen for one request, with the values that apply to its workflow.
#[derive(Clone, Debug)]
pub struct AppliedProfile {
    pub id: String,
    pub values: BTreeMap<String, Value>,
}

/// User overrides with the profile values beneath them: precedence is
/// default < profile < preset < user. `overrides` feeds binding unchanged.
#[derive(Clone, Debug)]
pub struct LayeredBindings {
    pub overrides: BTreeMap<String, Value>,
    pub profile: Option<AppliedProfile>,
}

impl Harness {
    /// The pack that declares `workflow`, if any. Scans the file system.
    pub fn pack_of(&self, workflow: &LoadedWorkflow) -> Option<LoadedPack> {
        let path = std::fs::canonicalize(&workflow.manifest_path).ok()?;
        self.discover_packs().packs.into_iter().find(|pack| {
            pack.workflow_paths
                .iter()
                .any(|declared| *declared == path || *declared == workflow.manifest_path)
        })
    }

    /// The profiles of the workflow's pack; empty for a user workflow.
    pub fn checkpoint_profiles_of(&self, workflow: &LoadedWorkflow) -> Vec<CheckpointProfile> {
        self.pack_of(workflow)
            .map(|pack| pack.checkpoint_profiles)
            .unwrap_or_default()
    }

    /// Layers the named profile, if any, beneath the preset and user values.
    pub fn layer_bindings(
        &self,
        workflow: &LoadedWorkflow,
        preset: Option<&str>,
        profile: Option<&str>,
        user: &BTreeMap<String, Value>,
    ) -> Result<LayeredBindings> {
        let Some(profile_id) = profile else {
            return Ok(LayeredBindings {
                overrides: user.clone(),
                profile: None,
            });
        };
        let fail = |reason: String| HarnessError::Workflow(reason);
        let workflow_id = &workflow.manifest.id;
        let pack = self.pack_of(workflow).ok_or_else(|| {
            fail(format!(
                "checkpoint profile '{profile_id}' was requested, but workflow '{workflow_id}' does not belong to a pack; only pack workflows have checkpoint profiles"
            ))
        })?;
        let chosen = pack
            .checkpoint_profiles
            .iter()
            .find(|candidate| candidate.id == profile_id)
            .ok_or_else(|| {
                fail(format!(
                    "pack '{}' has no checkpoint profile '{profile_id}'; list the profiles of workflow '{workflow_id}' and name one exactly",
                    pack.manifest.id
                ))
            })?;
        let checkpoint_parameter = workflow.checkpoint_parameter().ok_or_else(|| {
            fail(format!(
                "workflow '{workflow_id}' has no single checkpoint parameter, so checkpoint profile '{profile_id}' cannot be matched to a checkpoint"
            ))
        })?;
        let preset_values = workflow.preset_values(preset)?;
        let checkpoint = user
            .get(checkpoint_parameter)
            .or_else(|| preset_values.get(checkpoint_parameter))
            .or_else(|| workflow.manifest.parameters[checkpoint_parameter].default.as_ref())
            .and_then(Value::as_str)
            .ok_or_else(|| {
                fail(format!(
                    "checkpoint profile '{profile_id}' needs an explicit value for '{checkpoint_parameter}'; none was given"
                ))
            })?;
        if !chosen.matches(checkpoint) {
            return Err(fail(format!(
                "checkpoint profile '{profile_id}' does not list checkpoint '{checkpoint}' (matching is exact); it lists: {}",
                chosen.checkpoints.join(", ")
            )));
        }
        let values = chosen.values_for(&workflow.manifest);
        if values.contains_key(checkpoint_parameter) {
            return Err(fail(format!(
                "checkpoint profile '{profile_id}' must not set the checkpoint parameter '{checkpoint_parameter}'"
            )));
        }
        let mut overrides: BTreeMap<String, Value> = values
            .iter()
            .filter(|(name, _)| !preset_values.contains_key(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        overrides.extend(
            user.iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        Ok(LayeredBindings {
            overrides,
            profile: Some(AppliedProfile {
                id: chosen.id.clone(),
                values,
            }),
        })
    }
}
