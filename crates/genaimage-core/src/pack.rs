//! Workflow packs: read-only directories that bundle workflow manifests,
//! template references and declared requirements.
//!
//! A pack root is a directory of pack directories. Each immediate child that
//! contains `pack.toml` is a pack. Discovery is bounded, isolates broken packs
//! and never installs or substitutes anything.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    checkpoint_profiles::CheckpointProfile, workflow::read_regular_file_limited, Harness,
    HarnessError, LoadedWorkflow, Result,
};

pub const PACK_FILE: &str = "pack.toml";
pub const MAX_PACK_FILE_BYTES: u64 = 64 * 1024;
pub const MAX_PACKS: usize = 64;
pub const MAX_PACK_WORKFLOWS: usize = 64;
pub const MAX_PACK_TEMPLATES: usize = 64;
pub const MAX_NODE_CLASSES: usize = 256;
pub const MAX_MODEL_FOLDERS: usize = 32;
pub const MAX_MODELS: usize = 512;
pub const MAX_RELATIVE_COMPONENTS: usize = 6;
const MAX_ID_LEN: usize = 64;
const MAX_TEXT_LEN: usize = 256;
const MAX_DESCRIPTION_LEN: usize = 2000;
const MAX_PATH_LEN: usize = 240;
const MAX_ROOT_ENTRIES: usize = 512;

/// Declared requirements. They are checked against the backend, never installed.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackRequires {
    pub node_classes: Vec<String>,
    /// Model folder (`checkpoints`, `loras`, ...) to file names.
    pub models: BTreeMap<String, Vec<String>>,
}

impl PackRequires {
    pub fn is_empty(&self) -> bool {
        self.node_classes.is_empty() && self.models.values().all(Vec::is_empty)
    }
}

/// The strict contents of `pack.toml`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Manifest paths relative to the pack, each ending in `.genaimage.json`.
    #[serde(default)]
    pub workflows: Vec<String>,
    /// Template paths relative to the pack. Listed only; templates are consumed elsewhere.
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub requires: PackRequires,
    /// Optional TOML file of checkpoint profiles, relative to the pack.
    #[serde(default)]
    pub checkpoint_profiles: Option<String>,
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Pack(message.into())
}

/// Lowercase ASCII letters, digits, `-` and `_`; starts with a letter or digit.
pub fn validate_pack_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let valid = id.len() <= MAX_ID_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "id '{id}' must be 1 to {MAX_ID_LEN} lowercase letters, digits, '-' or '_' and start with a letter or digit"
        )))
    }
}

fn validate_relative(kind: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    let components: Vec<_> = path.components().collect();
    let safe = !value.is_empty()
        && value.len() <= MAX_PATH_LEN
        && !value.contains(['\\', '\0'])
        && !components.is_empty()
        && components.len() <= MAX_RELATIVE_COMPONENTS
        && components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)));
    if safe {
        Ok(())
    } else {
        Err(invalid(format!(
            "{kind} path '{}' must be a relative path of at most {MAX_RELATIVE_COMPONENTS} plain components inside the pack (no '..', absolute paths or backslashes)",
            value.escape_debug()
        )))
    }
}

fn validate_text(kind: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > MAX_TEXT_LEN || value.contains(['\0', '\n']) {
        return Err(invalid(format!(
            "{kind} '{}' must be 1 to {MAX_TEXT_LEN} bytes on one line",
            value.escape_debug()
        )));
    }
    Ok(())
}

fn check_unique<'a>(kind: &str, items: impl Iterator<Item = &'a String>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for item in items {
        if !seen.insert(item) {
            return Err(invalid(format!("{kind} '{item}' is listed twice")));
        }
    }
    Ok(())
}

fn check_count(kind: &str, count: usize, max: usize) -> Result<()> {
    if count > max {
        return Err(invalid(format!(
            "{kind}: at most {max} entries, found {count}"
        )));
    }
    Ok(())
}

impl PackManifest {
    /// Parses and validates `pack.toml` text.
    pub fn parse(source: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(source).map_err(|e| invalid(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        validate_pack_id(&self.id)?;
        validate_text("version", &self.version)?;
        if self.description.len() > MAX_DESCRIPTION_LEN {
            return Err(invalid(format!(
                "description is longer than {MAX_DESCRIPTION_LEN} bytes"
            )));
        }
        check_count("workflows", self.workflows.len(), MAX_PACK_WORKFLOWS)?;
        check_count("templates", self.templates.len(), MAX_PACK_TEMPLATES)?;
        for path in &self.workflows {
            validate_relative("workflow", path)?;
            if !path.ends_with(".genaimage.json") {
                return Err(invalid(format!(
                    "workflow path '{path}' must end in .genaimage.json"
                )));
            }
        }
        for path in &self.templates {
            validate_relative("template", path)?;
        }
        if let Some(path) = &self.checkpoint_profiles {
            validate_relative("checkpoint profiles", path)?;
            if !path.ends_with(".toml") {
                return Err(invalid(format!(
                    "checkpoint profiles path '{path}' must end in .toml"
                )));
            }
        }
        check_unique("workflow", self.workflows.iter())?;
        check_unique("template", self.templates.iter())?;
        let requires = &self.requires;
        check_count(
            "requires.node_classes",
            requires.node_classes.len(),
            MAX_NODE_CLASSES,
        )?;
        check_count(
            "requires.models folders",
            requires.models.len(),
            MAX_MODEL_FOLDERS,
        )?;
        check_count(
            "requires.models",
            requires.models.values().map(Vec::len).sum(),
            MAX_MODELS,
        )?;
        for class in &requires.node_classes {
            validate_text("node class", class)?;
        }
        check_unique("node class", requires.node_classes.iter())?;
        for (folder, names) in &requires.models {
            let folder_ok = !folder.is_empty()
                && folder.len() <= 64
                && folder
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !folder_ok {
                return Err(invalid(format!(
                    "model folder '{}' must be lowercase letters, digits and '_'",
                    folder.escape_debug()
                )));
            }
            for name in names {
                validate_text("model name", name)?;
            }
            check_unique("model name", names.iter())?;
        }
        Ok(())
    }
}

/// A template reference declared by a pack, with its resolved file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PackTemplate {
    pub pack_id: String,
    /// The path as declared, relative to the pack.
    pub path: String,
    pub file: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

impl PackDiagnostic {
    fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

/// A valid pack with every declared path confined to its directory.
#[derive(Clone, Debug)]
pub struct LoadedPack {
    pub manifest: PackManifest,
    /// The pack directory as found below its root.
    pub dir: PathBuf,
    /// The resolved pack directory; every declared file resolves inside it.
    pub canonical_dir: PathBuf,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    /// Existing workflow manifest files, as canonical paths inside the pack directory.
    pub workflow_paths: Vec<PathBuf>,
    pub templates: Vec<PackTemplate>,
    /// Profiles from the declared file; empty when it is absent or invalid.
    pub checkpoint_profiles: Vec<CheckpointProfile>,
    /// Declared files that are missing or unreadable; the pack still loads.
    pub diagnostics: Vec<PackDiagnostic>,
}

enum Confined {
    Inside(PathBuf),
    Missing,
    Unreadable(String),
}

/// Resolves `relative` below `dir`, rejecting symlinks that lead outside it.
/// The canonical path is returned so that the file read is the file checked.
fn confine(canonical_dir: &Path, dir: &Path, relative: &str) -> Result<Confined> {
    match fs::canonicalize(dir.join(relative)) {
        Ok(real) if real.starts_with(canonical_dir) => Ok(Confined::Inside(real)),
        Ok(_) => Err(invalid(format!(
            "'{relative}' resolves outside the pack directory through a symlink"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Confined::Missing),
        Err(error) => Ok(Confined::Unreadable(error.to_string())),
    }
}

impl LoadedPack {
    /// Reads and validates the pack in `dir`, which must contain `pack.toml`.
    pub fn load(root: &Path, dir: &Path) -> Result<Self> {
        let manifest_path = dir.join(PACK_FILE);
        let canonical = fs::canonicalize(dir)
            .map_err(|error| invalid(format!("cannot resolve the pack directory: {error}")))?;
        let real_manifest = fs::canonicalize(&manifest_path)
            .map_err(|error| invalid(format!("cannot read {PACK_FILE}: {error}")))?;
        if !real_manifest.starts_with(&canonical) {
            return Err(invalid(format!(
                "{PACK_FILE} resolves outside the pack directory through a symlink and was not read"
            )));
        }
        let bytes = read_regular_file_limited(&real_manifest, MAX_PACK_FILE_BYTES)
            .map_err(|error| invalid(format!("cannot read {PACK_FILE}: {error}")))?;
        let source = String::from_utf8(bytes)
            .map_err(|_| invalid(format!("{PACK_FILE} is not valid UTF-8")))?;
        let manifest = PackManifest::parse(&source)?;
        let mut pack = Self {
            manifest,
            dir: dir.to_path_buf(),
            canonical_dir: canonical.clone(),
            root: root.to_path_buf(),
            manifest_path,
            workflow_paths: Vec::new(),
            templates: Vec::new(),
            checkpoint_profiles: Vec::new(),
            diagnostics: Vec::new(),
        };
        for relative in pack.manifest.workflows.clone() {
            match confine(&canonical, dir, &relative)? {
                Confined::Inside(path) => pack.workflow_paths.push(path),
                outcome => pack.declared_file_problem("workflow", &relative, dir, outcome),
            }
        }
        for relative in pack.manifest.templates.clone() {
            match confine(&canonical, dir, &relative)? {
                Confined::Inside(file) => pack.templates.push(PackTemplate {
                    pack_id: pack.manifest.id.clone(),
                    path: relative,
                    file,
                }),
                outcome => pack.declared_file_problem("template", &relative, dir, outcome),
            }
        }
        if let Some(relative) = pack.manifest.checkpoint_profiles.clone() {
            match confine(&canonical, dir, &relative)? {
                Confined::Inside(file) => match read_profiles(&file) {
                    Ok(profiles) => pack.checkpoint_profiles = profiles,
                    Err(reason) => pack.diagnostics.push(PackDiagnostic::new(
                        &dir.join(&relative),
                        format!(
                            "pack '{}' checkpoint profiles '{relative}' were not loaded: {reason}",
                            pack.manifest.id
                        ),
                    )),
                },
                outcome => {
                    pack.declared_file_problem("checkpoint profiles", &relative, dir, outcome)
                }
            }
        }
        Ok(pack)
    }

    /// Loads one declared workflow with its manifest and graph confined to the
    /// pack directory. Discovery and every listing use this one function.
    pub fn load_workflow(&self, manifest_path: &Path) -> Result<LoadedWorkflow> {
        LoadedWorkflow::load_within(manifest_path, &self.canonical_dir)
    }

    fn declared_file_problem(&mut self, kind: &str, relative: &str, dir: &Path, outcome: Confined) {
        let reason = match outcome {
            Confined::Unreadable(error) => format!("cannot be read ({error})"),
            _ => "does not exist".to_owned(),
        };
        self.diagnostics.push(PackDiagnostic::new(
            &dir.join(relative),
            format!(
                "pack '{}' declares {kind} '{relative}', which {reason}",
                self.manifest.id
            ),
        ));
    }
}

fn read_profiles(file: &Path) -> std::result::Result<Vec<CheckpointProfile>, String> {
    let bytes = read_regular_file_limited(file, MAX_PACK_FILE_BYTES).map_err(|e| e.to_string())?;
    let source = String::from_utf8(bytes).map_err(|_| "the file is not valid UTF-8".to_owned())?;
    crate::checkpoint_profiles::parse_profiles(&source)
}

#[derive(Clone, Debug, Default)]
pub struct PackDiscovery {
    pub packs: Vec<LoadedPack>,
    pub diagnostics: Vec<PackDiagnostic>,
}

impl PackDiscovery {
    pub fn pack(&self, id: &str) -> Option<&LoadedPack> {
        self.packs.iter().find(|pack| pack.manifest.id == id)
    }

    pub fn templates(&self) -> impl Iterator<Item = &PackTemplate> {
        self.packs.iter().flat_map(|pack| pack.templates.iter())
    }

    /// Every diagnostic, including those of declared files inside valid packs.
    pub fn all_diagnostics(&self) -> Vec<PackDiagnostic> {
        let mut all = self.diagnostics.clone();
        all.extend(self.packs.iter().flat_map(|pack| pack.diagnostics.clone()));
        all.sort_by(|a, b| a.path.cmp(&b.path));
        all
    }
}

/// Scans pack roots. Missing roots are skipped; unreadable or invalid packs
/// become diagnostics. Packs sharing an id are all rejected.
pub fn discover_packs(roots: &[PathBuf]) -> PackDiscovery {
    let mut found = Vec::new();
    let mut diagnostics = Vec::new();
    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                diagnostics.push(PackDiagnostic::new(
                    root,
                    format!("cannot read pack root: {error}"),
                ));
                continue;
            }
        };
        if root.join(PACK_FILE).exists() {
            diagnostics.push(PackDiagnostic::new(
                &root.join(PACK_FILE),
                "this directory is itself a pack; a pack root holds pack directories, so point `paths.packs` at its parent",
            ));
        }
        let mut names = Vec::new();
        for entry in entries.take(MAX_ROOT_ENTRIES + 1) {
            match entry {
                Ok(entry) => names.push(entry.path()),
                Err(error) => diagnostics.push(PackDiagnostic::new(
                    root,
                    format!("cannot read a pack root entry: {error}"),
                )),
            }
        }
        if names.len() > MAX_ROOT_ENTRIES {
            names.truncate(MAX_ROOT_ENTRIES);
            diagnostics.push(PackDiagnostic::new(
                root,
                format!(
                    "pack root has more than {MAX_ROOT_ENTRIES} entries; the rest were not scanned"
                ),
            ));
        }
        names.sort();
        for dir in names {
            if !fs::metadata(&dir).is_ok_and(|metadata| metadata.is_dir()) {
                continue;
            }
            let manifest = dir.join(PACK_FILE);
            if matches!(fs::symlink_metadata(&manifest), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            match LoadedPack::load(root, &dir) {
                Ok(pack) => found.push(pack),
                Err(HarnessError::Pack(message)) => {
                    diagnostics.push(PackDiagnostic::new(&manifest, message))
                }
                Err(error) => diagnostics.push(PackDiagnostic::new(&manifest, error.to_string())),
            }
        }
    }
    let mut by_id: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for pack in &found {
        by_id
            .entry(pack.manifest.id.clone())
            .or_default()
            .push(pack.manifest_path.clone());
    }
    let mut packs = Vec::new();
    for pack in found {
        let paths = &by_id[&pack.manifest.id];
        if paths.len() > 1 {
            diagnostics.push(PackDiagnostic::new(
                &pack.manifest_path,
                format!(
                    "pack id '{}' is declared by {} packs ({}); none of them is available until the duplicates are removed",
                    pack.manifest.id,
                    paths.len(),
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        } else if packs.len() < MAX_PACKS {
            packs.push(pack);
        } else {
            diagnostics.push(PackDiagnostic::new(
                &pack.manifest_path,
                format!("more than {MAX_PACKS} packs; this one was not loaded"),
            ));
        }
    }
    diagnostics.sort_by(|a, b| a.path.cmp(&b.path));
    packs.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    PackDiscovery { packs, diagnostics }
}

impl Harness {
    /// Discovers packs below `paths.packs`. This scans the file system.
    pub fn discover_packs(&self) -> PackDiscovery {
        discover_packs(&self.config().packs)
    }

    /// Templates declared by every valid pack. They are listed, not interpreted.
    pub fn list_pack_templates(&self) -> Vec<PackTemplate> {
        self.discover_packs().templates().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
id = "demo"
version = "1.2.3"
description = "A demo pack."
workflows = ["flows/a.genaimage.json"]
templates = ["templates/portrait.md"]

[requires]
node_classes = ["KSampler", "LoraLoader"]

[requires.models]
checkpoints = ["base.safetensors"]
loras = ["style.safetensors"]
"#;

    fn parse_error(source: &str) -> String {
        PackManifest::parse(source).unwrap_err().to_string()
    }

    #[test]
    fn parses_every_declared_field() {
        let manifest = PackManifest::parse(VALID).unwrap();
        assert_eq!(manifest.id, "demo");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.workflows, ["flows/a.genaimage.json"]);
        assert_eq!(manifest.templates, ["templates/portrait.md"]);
        assert_eq!(manifest.requires.node_classes, ["KSampler", "LoraLoader"]);
        assert_eq!(manifest.requires.models["loras"], ["style.safetensors"]);
    }

    #[test]
    fn minimal_manifest_defaults_to_empty_lists() {
        let manifest = PackManifest::parse("id = \"m\"\nversion = \"1\"\n").unwrap();
        assert!(manifest.workflows.is_empty() && manifest.requires.is_empty());
        assert_eq!(manifest.description, "");
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        for source in [
            "id='a'\nversion='1'\nextra=1\n",
            "id='a'\nversion='1'\n[requires]\nsurprise=[]\n",
        ] {
            assert!(parse_error(source).contains("unknown field"), "{source}");
        }
    }

    #[test]
    fn versions_are_text_never_floats() {
        assert!(PackManifest::parse("id='a'\nversion=1.0\n").is_err());
    }

    #[test]
    fn rejects_invalid_ids() {
        for id in [
            "",
            "Upper",
            "-lead",
            "has space",
            "a/b",
            "..",
            &"x".repeat(65),
        ] {
            let source = format!("id = {id:?}\nversion = '1'\n");
            assert!(PackManifest::parse(&source).is_err(), "{id:?}");
        }
    }

    #[test]
    fn rejects_paths_that_leave_the_pack() {
        for path in [
            "../x.genaimage.json",
            "a/../../x.genaimage.json",
            "/etc/x.genaimage.json",
            "a\\b.genaimage.json",
            "",
            "a/b/c/d/e/f/g.genaimage.json",
        ] {
            let source = format!("id='a'\nversion='1'\nworkflows=[{path:?}]\n");
            assert!(PackManifest::parse(&source).is_err(), "{path:?}");
        }
        assert!(PackManifest::parse("id='a'\nversion='1'\ntemplates=['../t']\n").is_err());
        assert!(PackManifest::parse("id='a'\nversion='1'\nworkflows=['x.json']\n").is_err());
    }

    #[test]
    fn rejects_duplicates_and_bad_requirements() {
        for body in [
            "workflows=['a.genaimage.json','a.genaimage.json']",
            "[requires]\nnode_classes=['A','A']",
            "[requires]\nnode_classes=['']",
            "[requires.models]\nBad-Folder=['x']",
            "[requires.models]\nloras=['x','x']",
        ] {
            let source = format!("id='a'\nversion='1'\n{body}\n");
            assert!(PackManifest::parse(&source).is_err(), "{body}");
        }
    }

    #[test]
    fn bounds_the_number_of_entries() {
        let names = (0..=MAX_NODE_CLASSES)
            .map(|index| format!("'N{index}'"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("id='a'\nversion='1'\n[requires]\nnode_classes=[{names}]\n");
        assert!(parse_error(&source).contains("at most"));
    }

    fn write_pack(root: &Path, dir: &str, source: &str) -> PathBuf {
        let pack = root.join(dir);
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join(PACK_FILE), source).unwrap();
        pack
    }

    #[test]
    fn oversized_pack_files_are_rejected_without_being_parsed() {
        let temp = tempfile::tempdir().unwrap();
        let padding = " ".repeat(MAX_PACK_FILE_BYTES as usize + 1);
        let dir = write_pack(
            temp.path(),
            "big",
            &format!("id='big'\nversion='1'\n#{padding}"),
        );
        let error = LoadedPack::load(temp.path(), &dir).unwrap_err().to_string();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn loads_confined_files_and_reports_missing_declared_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = write_pack(temp.path(), "demo", VALID);
        fs::create_dir_all(dir.join("flows")).unwrap();
        fs::write(dir.join("flows/a.genaimage.json"), "{}").unwrap();
        let pack = LoadedPack::load(temp.path(), &dir).unwrap();
        assert_eq!(pack.workflow_paths, [dir.join("flows/a.genaimage.json")]);
        assert!(pack.templates.is_empty());
        assert_eq!(pack.diagnostics.len(), 1);
        assert!(pack.diagnostics[0]
            .message
            .contains("templates/portrait.md"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_that_escape_the_pack_reject_it() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("x.genaimage.json"), "{}").unwrap();
        fs::write(outside.join("t.md"), "x").unwrap();
        let root = temp.path().join("root");
        let dir = write_pack(
            &root,
            "evil",
            "id='evil'\nversion='1'\nworkflows=['link/x.genaimage.json']\n",
        );
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let error = LoadedPack::load(&root, &dir).unwrap_err().to_string();
        assert!(error.contains("outside the pack"), "{error}");
        fs::write(
            dir.join(PACK_FILE),
            "id='evil'\nversion='1'\ntemplates=['t.md']\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.join("t.md"), dir.join("t.md")).unwrap();
        assert!(LoadedPack::load(&root, &dir).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_pack_toml_symlinked_outside_the_pack_is_never_read_or_echoed() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        fs::write(&secret, "TOPSECRET = not toml").unwrap();
        let root = temp.path().join("root");
        let dir = root.join("evil");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&secret, dir.join(PACK_FILE)).unwrap();
        let error = LoadedPack::load(&root, &dir).unwrap_err().to_string();
        assert!(error.contains("outside the pack directory"), "{error}");
        assert!(!error.contains("TOPSECRET"), "{error}");
        let found = discover_packs(&[root]);
        assert!(found.packs.is_empty());
        let message = &found.diagnostics[0].message;
        assert!(!message.contains("TOPSECRET"), "{message}");
        assert!(!message.contains("workflow pack invalid"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pack_toml_symlink_inside_the_pack_is_read_from_its_canonical_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let dir = root.join("ok");
        fs::create_dir_all(dir.join("meta")).unwrap();
        fs::write(dir.join("meta/real.toml"), "id='ok'\nversion='1'\n").unwrap();
        std::os::unix::fs::symlink("meta/real.toml", dir.join(PACK_FILE)).unwrap();
        assert_eq!(LoadedPack::load(&root, &dir).unwrap().manifest.id, "ok");
    }

    #[test]
    fn a_broken_pack_does_not_hide_the_others() {
        let temp = tempfile::tempdir().unwrap();
        let (one, two) = (temp.path().join("one"), temp.path().join("two"));
        write_pack(&one, "good", "id='good'\nversion='1'\n");
        write_pack(&one, "bad", "id='bad'\nversion='1'\nunknown=true\n");
        write_pack(&one, "empty-id", "id=''\nversion='1'\n");
        fs::create_dir_all(one.join("not-a-pack")).unwrap();
        fs::write(one.join("stray.txt"), "x").unwrap();
        write_pack(&two, "other", "id='other'\nversion='2'\n");
        let found = discover_packs(&[one.clone(), temp.path().join("missing"), two]);
        let ids: Vec<_> = found.packs.iter().map(|p| p.manifest.id.as_str()).collect();
        assert_eq!(ids, ["good", "other"]);
        assert_eq!(found.diagnostics.len(), 2);
        assert!(found
            .diagnostics
            .iter()
            .all(|item| item.path.starts_with(&one)));
    }

    #[test]
    fn duplicate_pack_ids_make_every_claimant_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let (one, two) = (temp.path().join("one"), temp.path().join("two"));
        write_pack(&one, "a", "id='twin'\nversion='1'\n");
        write_pack(&two, "b", "id='twin'\nversion='2'\n");
        write_pack(&two, "c", "id='solo'\nversion='1'\n");
        let found = discover_packs(&[one, two]);
        assert!(found.pack("twin").is_none());
        assert!(found.pack("solo").is_some());
        assert_eq!(found.diagnostics.len(), 2);
        assert!(found.diagnostics[0]
            .message
            .contains("none of them is available"));
    }

    #[test]
    fn a_pack_directory_used_as_a_root_is_explained() {
        let temp = tempfile::tempdir().unwrap();
        let dir = write_pack(temp.path(), "p", "id='p'\nversion='1'\n");
        let found = discover_packs(&[dir]);
        assert!(found.packs.is_empty());
        assert!(found.diagnostics[0].message.contains("itself a pack"));
    }
}
