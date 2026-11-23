use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{HarnessError, Result};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub title: String,
    pub workflow: PathBuf,
    #[serde(default)]
    pub generation_mode: GenerationMode,
    #[serde(default)]
    pub parameters: BTreeMap<String, ParameterDefinition>,
    #[serde(default)]
    pub presets: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    pub outputs: Vec<OutputDefinition>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationMode {
    #[default]
    Generic,
    InitialConcept,
    Correction,
    Refinement,
    MultiView,
    Delivery,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterDefinition {
    pub node: String,
    pub input: String,
    pub kind: ParameterKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub allowed_values: Option<Vec<Value>>,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    String,
    Integer,
    Number,
    Boolean,
    Seed,
    ImageUpload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDefinition {
    pub node: String,
    #[serde(default = "default_output_kind")]
    pub kind: String,
}

fn default_output_kind() -> String {
    "image".to_owned()
}

#[derive(Clone, Debug)]
pub struct LoadedWorkflow {
    pub manifest_path: PathBuf,
    pub workflow_path: PathBuf,
    pub manifest: WorkflowManifest,
    pub workflow: Value,
    pub workflow_sha256: String,
}

impl LoadedWorkflow {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest_bytes = fs::read(manifest_path)?;
        let manifest: WorkflowManifest = serde_json::from_slice(&manifest_bytes)?;
        if manifest.schema_version != MANIFEST_VERSION {
            return Err(HarnessError::Workflow(format!(
                "manifest '{}' uses schema version {}, expected {}",
                manifest_path.display(),
                manifest.schema_version,
                MANIFEST_VERSION
            )));
        }
        if manifest.id.trim().is_empty() || manifest.version.trim().is_empty() {
            return Err(HarnessError::Workflow(
                "workflow id and version cannot be empty".to_owned(),
            ));
        }
        let workflow_path = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&manifest.workflow);
        let workflow_bytes = fs::read(&workflow_path).map_err(|error| {
            HarnessError::Workflow(format!(
                "cannot read workflow '{}': {error}",
                workflow_path.display()
            ))
        })?;
        let workflow: Value = serde_json::from_slice(&workflow_bytes).map_err(|error| {
            HarnessError::Workflow(format!(
                "workflow '{}' is not valid JSON: {error}",
                workflow_path.display()
            ))
        })?;
        let loaded = Self {
            manifest_path: manifest_path.to_path_buf(),
            workflow_path,
            manifest,
            workflow_sha256: sha256_hex(&workflow_bytes),
            workflow,
        };
        loaded.validate()?;
        Ok(loaded)
    }

    pub fn parameter_summary(&self) -> Vec<ParameterSummary> {
        self.manifest
            .parameters
            .iter()
            .map(|(name, definition)| ParameterSummary {
                name: name.clone(),
                kind: definition.kind,
                required: definition.required && definition.default.is_none(),
                description: definition.description.clone(),
                default: definition.default.clone(),
                allowed_values: definition.allowed_values.clone(),
            })
            .collect()
    }

    pub fn preset_values(&self, preset: Option<&str>) -> Result<BTreeMap<String, Value>> {
        match preset {
            None => Ok(BTreeMap::new()),
            Some(name) => self.manifest.presets.get(name).cloned().ok_or_else(|| {
                HarnessError::Workflow(format!(
                    "preset '{name}' is not defined by workflow '{}'",
                    self.manifest.id
                ))
            }),
        }
    }

    pub fn bind(
        &self,
        preset: Option<&str>,
        overrides: &BTreeMap<String, Value>,
    ) -> Result<BoundWorkflow> {
        let mut values = self.preset_values(preset)?;
        for (name, value) in overrides {
            if !self.manifest.parameters.contains_key(name) {
                return Err(HarnessError::Workflow(format!(
                    "parameter '{name}' is not defined by workflow '{}'",
                    self.manifest.id
                )));
            }
            values.insert(name.clone(), value.clone());
        }

        let mut workflow = self.workflow.clone();
        let nodes = workflow.as_object_mut().ok_or_else(|| {
            HarnessError::Workflow("API workflow root must be an object".to_owned())
        })?;
        let mut resolved = BTreeMap::new();
        let mut upload_paths = BTreeMap::new();
        for (name, definition) in &self.manifest.parameters {
            let value = values
                .get(name)
                .cloned()
                .or_else(|| definition.default.clone());
            let value = match value {
                Some(value) => value,
                None if definition.required => {
                    return Err(HarnessError::Workflow(format!(
                        "required parameter '{name}' has no value"
                    )));
                }
                None => continue,
            };
            definition.validate_value(name, &value)?;
            if definition.kind == ParameterKind::ImageUpload {
                let path = value.as_str().ok_or_else(|| {
                    HarnessError::Workflow(format!(
                        "image-upload parameter '{name}' must be a path string"
                    ))
                })?;
                upload_paths.insert(name.clone(), PathBuf::from(path));
                continue;
            }
            set_node_input(
                nodes,
                &definition.node,
                &definition.input,
                value.clone(),
                name,
            )?;
            resolved.insert(name.clone(), value);
        }

        Ok(BoundWorkflow {
            workflow,
            parameters: resolved,
            image_uploads: upload_paths,
        })
    }

    pub fn apply_uploaded_images(
        &self,
        bound: &mut BoundWorkflow,
        uploaded_names: &BTreeMap<String, String>,
    ) -> Result<()> {
        let nodes = bound.workflow.as_object_mut().ok_or_else(|| {
            HarnessError::Workflow("bound API workflow root is not an object".to_owned())
        })?;
        for (parameter, path) in &bound.image_uploads {
            let definition = self.manifest.parameters.get(parameter).ok_or_else(|| {
                HarnessError::Workflow(format!(
                    "upload parameter '{parameter}' disappeared from the manifest"
                ))
            })?;
            let uploaded = uploaded_names.get(parameter).ok_or_else(|| {
                HarnessError::Workflow(format!(
                    "image upload '{}' has not been uploaded",
                    path.display()
                ))
            })?;
            set_node_input(
                nodes,
                &definition.node,
                &definition.input,
                Value::String(uploaded.clone()),
                parameter,
            )?;
            bound
                .parameters
                .insert(parameter.clone(), Value::String(path.display().to_string()));
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if !self
            .manifest
            .workflow
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            return Err(HarnessError::Workflow(
                "ComfyUI workflows must be JSON files".to_owned(),
            ));
        }
        let nodes = self.workflow.as_object().ok_or_else(|| {
            HarnessError::Workflow(
                "API workflow root must be an object keyed by node ID; export the workflow in API format"
                    .to_owned(),
            )
        })?;
        if nodes.contains_key("nodes") || nodes.contains_key("links") {
            return Err(HarnessError::Workflow(
                "the file is a ComfyUI UI export; enable developer mode and export in API format"
                    .to_owned(),
            ));
        }
        for (name, parameter) in &self.manifest.parameters {
            validate_node_input(nodes, &parameter.node, &parameter.input, name)?;
            if let Some(default) = &parameter.default {
                parameter.validate_value(name, default)?;
            }
        }
        for name in self.manifest.presets.keys() {
            self.validate_values(
                self.manifest
                    .presets
                    .get(name)
                    .expect("preset exists because it was iterated"),
                &format!("preset '{name}'"),
            )?;
        }
        let mut bindings = BTreeSet::new();
        for (name, parameter) in &self.manifest.parameters {
            let binding = (parameter.node.clone(), parameter.input.clone());
            if !bindings.insert(binding) {
                return Err(HarnessError::Workflow(format!(
                    "parameter '{name}' targets a node input already bound by another parameter"
                )));
            }
        }
        for output in &self.manifest.outputs {
            validate_node(nodes, &output.node, "output")?;
        }
        Ok(())
    }

    fn validate_values(&self, values: &BTreeMap<String, Value>, context: &str) -> Result<()> {
        for (name, value) in values {
            let parameter = self.manifest.parameters.get(name).ok_or_else(|| {
                HarnessError::Workflow(format!("{context} sets unknown parameter '{name}'"))
            })?;
            parameter.validate_value(name, value)?;
        }
        Ok(())
    }
}

impl ParameterDefinition {
    pub fn validate_value(&self, name: &str, value: &Value) -> Result<()> {
        let valid_type = match self.kind {
            ParameterKind::String | ParameterKind::ImageUpload => value.is_string(),
            ParameterKind::Integer | ParameterKind::Seed => {
                value.as_i64().is_some() || value.as_u64().is_some()
            }
            ParameterKind::Number => value.is_number(),
            ParameterKind::Boolean => value.is_boolean(),
        };
        if !valid_type {
            return Err(HarnessError::Workflow(format!(
                "parameter '{name}' expects {:?}",
                self.kind
            )));
        }
        if let Some(allowed) = &self.allowed_values {
            if !allowed.contains(value) {
                return Err(HarnessError::Workflow(format!(
                    "parameter '{name}' value {value} is not in the allowed set"
                )));
            }
        }
        if let Some(number) = value.as_f64() {
            if let Some(minimum) = self.minimum {
                if number < minimum {
                    return Err(HarnessError::Workflow(format!(
                        "parameter '{name}' is below minimum {minimum}"
                    )));
                }
            }
            if let Some(maximum) = self.maximum {
                if number > maximum {
                    return Err(HarnessError::Workflow(format!(
                        "parameter '{name}' is above maximum {maximum}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct BoundWorkflow {
    pub workflow: Value,
    pub parameters: BTreeMap<String, Value>,
    pub image_uploads: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParameterSummary {
    pub name: String,
    pub kind: ParameterKind,
    pub required: bool,
    pub description: String,
    pub default: Option<Value>,
    pub allowed_values: Option<Vec<Value>>,
}

fn set_node_input(
    nodes: &mut Map<String, Value>,
    node_id: &str,
    input: &str,
    value: Value,
    parameter: &str,
) -> Result<()> {
    let node = nodes.get_mut(node_id).ok_or_else(|| {
        HarnessError::Workflow(format!(
            "parameter '{parameter}' targets missing node '{node_id}'"
        ))
    })?;
    let node = node.as_object_mut().ok_or_else(|| {
        HarnessError::Workflow(format!(
            "parameter '{parameter}' targets node '{node_id}', which is not an object"
        ))
    })?;
    let inputs = node
        .get_mut("inputs")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            HarnessError::Workflow(format!(
                "node '{node_id}' has no inputs object for parameter '{parameter}'"
            ))
        })?;
    if !inputs.contains_key(input) {
        return Err(HarnessError::Workflow(format!(
            "parameter '{parameter}' targets missing input '{input}' on node '{node_id}'"
        )));
    }
    inputs.insert(input.to_owned(), value);
    Ok(())
}

fn validate_node_input(
    nodes: &Map<String, Value>,
    node_id: &str,
    input: &str,
    parameter: &str,
) -> Result<()> {
    validate_node(nodes, node_id, parameter)?;
    let inputs = nodes[node_id]
        .get("inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            HarnessError::Workflow(format!(
                "node '{node_id}' has no inputs object for parameter '{parameter}'"
            ))
        })?;
    if !inputs.contains_key(input) {
        return Err(HarnessError::Workflow(format!(
            "parameter '{parameter}' targets missing input '{input}' on node '{node_id}'"
        )));
    }
    Ok(())
}

fn validate_node(nodes: &Map<String, Value>, node_id: &str, context: &str) -> Result<()> {
    if !nodes.contains_key(node_id) {
        return Err(HarnessError::Workflow(format!(
            "{context} targets missing node '{node_id}'"
        )));
    }
    if !nodes[node_id].is_object() {
        return Err(HarnessError::Workflow(format!(
            "{context} targets node '{node_id}', which is not an object"
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
