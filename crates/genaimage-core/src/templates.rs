//! Prompt template library: deterministic `{{name}}` text templates.
//!
//! A template is one strict TOML file. User templates live in
//! `paths.templates` as `<id>.toml` and are writable; pack templates are
//! declared by `pack.toml`, confined to their pack and read-only. Rendering is
//! pure string substitution: no logic, no filters, no nesting, no trimming and
//! no LLM. Every template is identified by the SHA-256 of its file bytes, and
//! writes and renders are pinned to the version the user reviewed.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{safe_file, workflow::read_regular_file_limited, Harness};

pub const MAX_ID_LENGTH: usize = 64;
pub const MAX_TITLE_BYTES: usize = 200;
pub const MAX_DESCRIPTION_BYTES: usize = 2000;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_VARIABLES: usize = 32;
pub const MAX_VALUE_BYTES: usize = 4 * 1024;
pub const MAX_RENDERED_BYTES: usize = 64 * 1024;
pub const MAX_FILE_BYTES: u64 = 128 * 1024;
/// Templates returned by one listing; the rest is reported in `diagnostics`.
pub const MAX_LISTED: usize = 256;
pub const MAX_DIAGNOSTICS: usize = 100;
const MAX_SCANNED_ENTRIES: usize = 1024;
const MAX_ISSUES: usize = 20;
const MAX_DIAGNOSTIC_CHARS: usize = 400;
const FILE_EXTENSION: &str = "toml";
const LOCK_FILE: &str = ".templates.lock";

/// Serialises writers within this process. Other processes are kept apart by
/// an advisory file lock in the template directory.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateScope {
    User,
    Pack,
}

/// One declared variable, as written in the template file.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VariableDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// The content of a template file. Unknown fields are rejected.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateDefinition {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, VariableDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TemplateVariable {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub default: Option<String>,
}

/// A valid, usable template.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Template {
    /// `user:<id>` or `pack:<pack_id>:<relative path>`.
    pub reference: String,
    pub scope: TemplateScope,
    pub pack_id: Option<String>,
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// Ordered by name.
    pub variables: Vec<TemplateVariable>,
    /// SHA-256 of the template file bytes.
    pub sha256: String,
    pub text: String,
}

/// An unusable template or file, reported instead of the template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateDiagnostic {
    pub reference: Option<String>,
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, Default)]
pub struct TemplateListing {
    pub templates: Vec<Template>,
    pub diagnostics: Vec<TemplateDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateIssue {
    pub key: String,
    pub reason: String,
}

fn issue(key: impl Into<String>, reason: impl Into<String>) -> TemplateIssue {
    TemplateIssue {
        key: key.into(),
        reason: reason.into(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedTemplate {
    pub text: String,
    pub reference: String,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error(
        "the template reference '{0}' is not valid; use 'user:<id>' or 'pack:<pack_id>:<path>'"
    )]
    InvalidRef(String),
    #[error("template '{0}' was not found")]
    NotFound(String),
    #[error("the template is invalid ({} problem(s))", .0.len())]
    Invalid(Vec<TemplateIssue>),
    #[error("template id '{0}' already exists; nothing was written")]
    Exists(String),
    #[error("the template changed since it was reviewed (current sha256 {current_sha256})")]
    Changed { current_sha256: String },
    #[error("pack templates are read-only")]
    ReadOnly,
    #[error("the template cannot be rendered ({} problem(s))", .0.len())]
    RenderInvalid(Vec<TemplateIssue>),
    #[error("template storage failed: {0}")]
    Io(String),
}

impl From<io::Error> for TemplateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

type Result<T, E = TemplateError> = std::result::Result<T, E>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateRef {
    User(String),
    Pack { pack_id: String, path: String },
}

impl TemplateRef {
    pub fn parse(reference: &str) -> Result<Self> {
        let bad = || TemplateError::InvalidRef(reference.chars().take(200).collect());
        if let Some(id) = reference.strip_prefix("user:") {
            return is_template_id(id)
                .then(|| Self::User(id.to_owned()))
                .ok_or_else(bad);
        }
        let rest = reference.strip_prefix("pack:").ok_or_else(bad)?;
        let (pack_id, path) = rest.split_once(':').ok_or_else(bad)?;
        if !is_template_id(pack_id) || path.is_empty() {
            return Err(bad());
        }
        Ok(Self::Pack {
            pack_id: pack_id.to_owned(),
            path: path.to_owned(),
        })
    }

    pub fn user(id: &str) -> String {
        format!("user:{id}")
    }

    pub fn pack(pack_id: &str, path: &str) -> String {
        format!("pack:{pack_id}:{path}")
    }
}

/// `[a-z0-9][a-z0-9_-]{0,63}`; also the pack ID rule.
pub fn is_template_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID_LENGTH
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// `[a-z][a-z0-9_]{0,63}`.
pub fn is_variable_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID_LENGTH
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Eq, PartialEq)]
enum Segment<'a> {
    Literal(String),
    Variable(&'a str),
}

/// Splits template text into literals and `{{name}}` placeholders.
///
/// `{{{{` is a literal `{{`. A `{{` that does not start `{{{{` must start a
/// complete `{{name}}`; anything else is malformed. Single braces and `}}`
/// outside a placeholder are literal text.
fn parse_text(text: &str) -> std::result::Result<Vec<Segment<'_>>, String> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut offset = 0;
    let mut rest = text;
    while let Some(position) = rest.find("{{") {
        literal.push_str(&rest[..position]);
        let at = offset + position;
        let after = &rest[position..];
        if let Some(tail) = after.strip_prefix("{{{{") {
            literal.push_str("{{");
            offset = at + 4;
            rest = tail;
            continue;
        }
        let body = &after[2..];
        let end = body
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(body.len());
        let name = &body[..end];
        if !body[end..].starts_with("}}") || !is_variable_name(name) {
            return Err(format!(
                "malformed '{{{{' at byte {at}: write a placeholder as {{{{name}}}} (lower-case letters, digits and '_', starting with a letter, at most 64 characters, no spaces) or a literal '{{{{' as '{{{{{{{{'"
            ));
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut literal)));
        }
        segments.push(Segment::Variable(name));
        offset = at + 2 + end + 2;
        rest = &body[end + 2..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

impl TemplateDefinition {
    /// Every problem with the definition, at most 20. `stem` is the file stem
    /// the `id` must equal, for user templates.
    pub fn problems(&self, stem: Option<&str>) -> Vec<TemplateIssue> {
        let mut found = Vec::new();
        if !is_template_id(&self.id) {
            found.push(issue(
                "id",
                "use 1-64 lower-case letters, digits, '-' or '_', starting with a letter or digit",
            ));
        } else if stem.is_some_and(|stem| stem != self.id) {
            found.push(issue(
                "id",
                format!("must equal the file name '{}'", stem.unwrap_or_default()),
            ));
        }
        if self.title.trim().is_empty() || self.title.len() > MAX_TITLE_BYTES {
            found.push(issue(
                "title",
                format!("must be non-blank text of at most {MAX_TITLE_BYTES} bytes"),
            ));
        }
        if self
            .description
            .as_ref()
            .is_some_and(|text| text.len() > MAX_DESCRIPTION_BYTES)
        {
            found.push(issue(
                "description",
                format!("is longer than {MAX_DESCRIPTION_BYTES} bytes"),
            ));
        }
        if self.text.is_empty() || self.text.len() > MAX_TEXT_BYTES {
            found.push(issue(
                "text",
                format!("must be 1 to {MAX_TEXT_BYTES} bytes"),
            ));
        }
        if self.variables.len() > MAX_VARIABLES {
            found.push(issue(
                "variables",
                format!(
                    "at most {MAX_VARIABLES} variables, found {}",
                    self.variables.len()
                ),
            ));
        }
        for (name, variable) in &self.variables {
            let key = format!("variables.{name}");
            if !is_variable_name(name) {
                found.push(issue(&key, "use a name of lower-case letters, digits and '_', starting with a letter, at most 64 characters"));
            }
            if variable
                .description
                .as_ref()
                .is_some_and(|text| text.len() > MAX_DESCRIPTION_BYTES)
            {
                found.push(issue(
                    &key,
                    format!("description is longer than {MAX_DESCRIPTION_BYTES} bytes"),
                ));
            }
            match (&variable.default, variable.required) {
                (Some(_), true) => found.push(issue(&key, "is required and also has a default; remove one")),
                (None, false) => found.push(issue(&key, "is optional without a default; set required = true or give a default (an empty default is allowed)")),
                _ => {}
            }
            if variable
                .default
                .as_ref()
                .is_some_and(|text| text.len() > MAX_VALUE_BYTES)
            {
                found.push(issue(
                    &key,
                    format!("default is longer than {MAX_VALUE_BYTES} bytes"),
                ));
            }
        }
        if self.text.len() <= MAX_TEXT_BYTES {
            match parse_text(&self.text) {
                Err(reason) => found.push(issue("text", reason)),
                Ok(segments) => {
                    let used: BTreeSet<&str> = segments
                        .iter()
                        .filter_map(|segment| match segment {
                            Segment::Variable(name) => Some(*name),
                            Segment::Literal(_) => None,
                        })
                        .collect();
                    for name in &used {
                        if !self.variables.contains_key(*name) {
                            found.push(issue("text", format!("uses the undeclared placeholder '{{{{{name}}}}}'; declare it under [variables.{name}]")));
                        }
                    }
                    for name in self.variables.keys() {
                        if !used.contains(name.as_str()) {
                            found.push(issue(
                                format!("variables.{name}"),
                                "is declared but not used in the text",
                            ));
                        }
                    }
                }
            }
        }
        found.truncate(MAX_ISSUES);
        found
    }

    fn to_file_bytes(&self) -> Result<Vec<u8>> {
        toml::to_string(self)
            .map(String::into_bytes)
            .map_err(|error| TemplateError::Io(error.to_string()))
    }
}

/// Parses and validates template file bytes into a usable template.
fn load(
    reference: String,
    scope: TemplateScope,
    pack_id: Option<String>,
    stem: Option<&str>,
    bytes: &[u8],
) -> std::result::Result<Template, Vec<TemplateIssue>> {
    let source =
        std::str::from_utf8(bytes).map_err(|_| vec![issue("file", "is not valid UTF-8")])?;
    let definition: TemplateDefinition = toml::from_str(source).map_err(|error| {
        vec![issue(
            "file",
            error
                .to_string()
                .chars()
                .take(MAX_DIAGNOSTIC_CHARS)
                .collect::<String>(),
        )]
    })?;
    let problems = definition.problems(stem);
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Template {
        reference,
        scope,
        pack_id,
        id: definition.id,
        title: definition.title,
        description: definition.description,
        variables: definition
            .variables
            .into_iter()
            .map(|(name, variable)| TemplateVariable {
                name,
                description: variable.description,
                required: variable.required,
                default: variable.default,
            })
            .collect(),
        sha256: sha256_hex(bytes),
        text: definition.text,
    })
}

fn describe(issues: &[TemplateIssue]) -> String {
    issues
        .iter()
        .map(|item| format!("{}: {}", item.key, item.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

impl Template {
    /// Substitutes `supplied` into the text. A variable that is absent gets its
    /// default; an empty string is a value, not absence. Values are inserted
    /// verbatim and never scanned again. Nothing is trimmed or normalised.
    pub fn render(&self, supplied: &Map<String, Value>) -> Result<String> {
        let mut problems = Vec::new();
        let mut values: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, value) in supplied {
            let Some(variable) = self.variables.iter().find(|v| &v.name == name) else {
                problems.push(issue(name, "is not a variable of this template"));
                continue;
            };
            match value.as_str() {
                None => problems.push(issue(name, "must be a string")),
                Some(text) if text.len() > MAX_VALUE_BYTES => problems.push(issue(
                    name,
                    format!("is longer than {MAX_VALUE_BYTES} bytes"),
                )),
                Some(text) => {
                    values.insert(&variable.name, text);
                }
            }
        }
        for variable in &self.variables {
            if supplied.contains_key(&variable.name) {
                continue;
            }
            match (&variable.default, variable.required) {
                (Some(default), _) => {
                    values.insert(&variable.name, default);
                }
                (None, _) => {
                    problems.push(issue(&variable.name, "is required and was not supplied"))
                }
            }
        }
        let segments = parse_text(&self.text)
            .map_err(|reason| TemplateError::Invalid(vec![issue("text", reason)]))?;
        let mut output = String::new();
        if problems.is_empty() {
            for segment in &segments {
                match segment {
                    Segment::Literal(text) => output.push_str(text),
                    Segment::Variable(name) => output.push_str(values[name]),
                }
                if output.len() > MAX_RENDERED_BYTES {
                    problems.push(issue(
                        "variables",
                        format!("the rendered text would exceed {MAX_RENDERED_BYTES} bytes"),
                    ));
                    break;
                }
            }
        }
        if problems.is_empty() {
            return Ok(output);
        }
        problems.sort_by(|a, b| a.key.cmp(&b.key));
        let total = problems.len();
        problems.truncate(MAX_ISSUES);
        if total > MAX_ISSUES {
            problems.push(issue(
                "",
                format!("{} more problems were omitted", total - MAX_ISSUES),
            ));
        }
        Err(TemplateError::RenderInvalid(problems))
    }
}

/// The writable template directory. Every operation validates and bounds its
/// own input; ids are validated before any path is built.
#[derive(Clone, Debug)]
pub struct UserTemplates {
    directory: PathBuf,
}

struct DirectoryLock {
    _guard: MutexGuard<'static, ()>,
    _file: fs::File,
}

fn file_name(id: &str) -> String {
    format!("{id}.{FILE_EXTENSION}")
}

fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(directory) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

fn write_temporary(directory: &Path, id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let temporary = directory.join(format!(".{id}.{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

impl UserTemplates {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn lock(&self, root: &Path) -> io::Result<DirectoryLock> {
        let guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(LOCK_FILE))?;
        #[cfg(unix)]
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        Ok(DirectoryLock {
            _guard: guard,
            _file: file,
        })
    }

    /// The canonical directory, or `None` when it does not exist yet.
    fn existing_root(&self) -> io::Result<Option<PathBuf>> {
        match fs::canonicalize(&self.directory) {
            Ok(root) => Ok(Some(root)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Reads one template file beneath the root without following symlinks.
    /// `None` when the file does not exist.
    fn read_file(root: &Path, id: &str) -> Result<Option<Vec<u8>>> {
        let name = file_name(id);
        match fs::symlink_metadata(root.join(&name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let mut file = safe_file::open_regular_beneath(root, root, Path::new(&name))
            .ok_or_else(|| TemplateError::Io(format!("'{name}' is not a regular file")))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(TemplateError::Io(format!(
                "'{name}' exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        Ok(Some(bytes))
    }

    fn user_template(root: &Path, id: &str) -> Result<Template> {
        let reference = TemplateRef::user(id);
        let bytes =
            Self::read_file(root, id)?.ok_or_else(|| TemplateError::NotFound(reference.clone()))?;
        load(reference, TemplateScope::User, None, Some(id), &bytes).map_err(TemplateError::Invalid)
    }

    pub fn get(&self, id: &str) -> Result<Template> {
        let reference = TemplateRef::user(id);
        if !is_template_id(id) {
            return Err(TemplateError::InvalidRef(reference));
        }
        let root = self
            .existing_root()?
            .ok_or_else(|| TemplateError::NotFound(reference.clone()))?;
        Self::user_template(&root, id)
    }

    /// Every valid template and a diagnostic for each file that is not usable.
    pub fn list(&self) -> TemplateListing {
        let mut listing = TemplateListing::default();
        let root = match self.existing_root() {
            Ok(Some(root)) => root,
            Ok(None) => return listing,
            Err(error) => {
                listing.diagnostics.push(TemplateDiagnostic {
                    reference: None,
                    path: self.directory.display().to_string(),
                    error: error.to_string(),
                });
                return listing;
            }
        };
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) => {
                listing.diagnostics.push(TemplateDiagnostic {
                    reference: None,
                    path: root.display().to_string(),
                    error: error.to_string(),
                });
                return listing;
            }
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.') && name.ends_with(".toml"))
            .take(MAX_SCANNED_ENTRIES)
            .collect();
        names.sort();
        for name in names {
            let path = root.join(&name).display().to_string();
            let stem = name.trim_end_matches(".toml");
            if !is_template_id(stem) {
                listing.diagnostics.push(TemplateDiagnostic {
                    reference: None,
                    path,
                    error: "the file name is not a valid template id; rename it to '<id>.toml' with a lower-case id".into(),
                });
                continue;
            }
            match Self::user_template(&root, stem) {
                Ok(template) => listing.templates.push(template),
                Err(error) => listing.diagnostics.push(TemplateDiagnostic {
                    reference: Some(TemplateRef::user(stem)),
                    path,
                    error: match error {
                        TemplateError::Invalid(issues) => describe(&issues),
                        other => other.to_string(),
                    },
                }),
            }
        }
        listing
    }

    fn checked(definition: &TemplateDefinition) -> Result<Vec<u8>> {
        let problems = definition.problems(None);
        if !problems.is_empty() {
            return Err(TemplateError::Invalid(problems));
        }
        let bytes = definition.to_file_bytes()?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(TemplateError::Invalid(vec![issue(
                "file",
                format!("is larger than {MAX_FILE_BYTES} bytes once written"),
            )]));
        }
        Ok(bytes)
    }

    fn reload(id: &str, bytes: &[u8]) -> Result<Template> {
        load(
            TemplateRef::user(id),
            TemplateScope::User,
            None,
            Some(id),
            bytes,
        )
        .map_err(TemplateError::Invalid)
    }

    /// Creates `<id>.toml`; an existing file is never touched.
    pub fn create(&self, definition: &TemplateDefinition) -> Result<Template> {
        let bytes = Self::checked(definition)?;
        fs::create_dir_all(&self.directory)?;
        let root = fs::canonicalize(&self.directory)?;
        let _lock = self.lock(&root)?;
        let id = &definition.id;
        let temporary = write_temporary(&root, id, &bytes)?;
        let target = root.join(file_name(id));
        let linked = fs::hard_link(&temporary, &target);
        let _ = fs::remove_file(&temporary);
        match linked {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(TemplateError::Exists(id.clone()))
            }
            Err(error) => {
                return Err(TemplateError::Io(format!(
                    "cannot create '{}' with a hard link ({error}); paths.templates must be on a filesystem that supports hard links",
                    target.display()
                )))
            }
        }
        sync_directory(&root);
        Self::reload(id, &bytes)
    }

    /// Reads the current file under the lock and compares its hash.
    fn locked_current(&self, id: &str, expected_sha256: &str) -> Result<(PathBuf, DirectoryLock)> {
        let reference = TemplateRef::user(id);
        if !is_template_id(id) {
            return Err(TemplateError::InvalidRef(reference));
        }
        if !is_sha256_hex(expected_sha256) {
            return Err(TemplateError::Invalid(vec![issue(
                "expected_sha256",
                "must be the 64-character lower-case hex sha256 of the template that was reviewed",
            )]));
        }
        let root = self
            .existing_root()?
            .ok_or_else(|| TemplateError::NotFound(reference.clone()))?;
        let lock = self.lock(&root)?;
        let current = Self::read_file(&root, id)?.ok_or(TemplateError::NotFound(reference))?;
        let current_sha256 = sha256_hex(&current);
        if current_sha256 != expected_sha256 {
            return Err(TemplateError::Changed { current_sha256 });
        }
        Ok((root, lock))
    }

    /// Atomically replaces `<id>.toml` when its current hash is `expected_sha256`.
    pub fn replace(
        &self,
        id: &str,
        expected_sha256: &str,
        definition: &TemplateDefinition,
    ) -> Result<Template> {
        if definition.id != id {
            return Err(TemplateError::Invalid(vec![issue(
                "id",
                "must equal the id in the URL; a template cannot be renamed",
            )]));
        }
        let bytes = Self::checked(definition)?;
        let (root, _lock) = self.locked_current(id, expected_sha256)?;
        let temporary = write_temporary(&root, id, &bytes)?;
        if let Err(error) = fs::rename(&temporary, root.join(file_name(id))) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        sync_directory(&root);
        Self::reload(id, &bytes)
    }

    /// Removes `<id>.toml` when its current hash is `expected_sha256`.
    pub fn delete(&self, id: &str, expected_sha256: &str) -> Result<()> {
        let (root, _lock) = self.locked_current(id, expected_sha256)?;
        fs::remove_file(root.join(file_name(id)))?;
        sync_directory(&root);
        Ok(())
    }

    /// The raw file hash and the parse result, read once.
    fn snapshot(&self, id: &str) -> Result<(String, Result<Template>)> {
        let reference = TemplateRef::user(id);
        let root = self
            .existing_root()?
            .ok_or_else(|| TemplateError::NotFound(reference.clone()))?;
        let bytes =
            Self::read_file(&root, id)?.ok_or(TemplateError::NotFound(reference.clone()))?;
        let sha256 = sha256_hex(&bytes);
        let template = load(reference, TemplateScope::User, None, Some(id), &bytes)
            .map_err(TemplateError::Invalid);
        Ok((sha256, template))
    }
}

fn pack_file(canonical_dir: &Path, file: &Path) -> io::Result<Vec<u8>> {
    if !file.starts_with(canonical_dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "resolves outside the pack directory",
        ));
    }
    read_regular_file_limited(file, MAX_FILE_BYTES)
}

impl Harness {
    fn user_templates(&self) -> UserTemplates {
        UserTemplates::new(&self.config().templates)
    }

    /// Valid user and pack templates, bounded to 256, plus a diagnostic for
    /// every unusable one. This reads the file system.
    pub fn list_templates(&self) -> TemplateListing {
        let mut listing = self.user_templates().list();
        let discovery = self.discover_packs();
        for pack in &discovery.packs {
            for declared in &pack.templates {
                let reference = TemplateRef::pack(&declared.pack_id, &declared.path);
                let loaded = pack_file(&pack.canonical_dir, &declared.file)
                    .map_err(|error| vec![issue("file", error.to_string())])
                    .and_then(|bytes| {
                        load(
                            reference.clone(),
                            TemplateScope::Pack,
                            Some(declared.pack_id.clone()),
                            None,
                            &bytes,
                        )
                    });
                match loaded {
                    Ok(template) => listing.templates.push(template),
                    Err(issues) => listing.diagnostics.push(TemplateDiagnostic {
                        reference: Some(reference),
                        path: declared.file.display().to_string(),
                        error: describe(&issues),
                    }),
                }
            }
        }
        if listing.templates.len() > MAX_LISTED {
            let more = listing.templates.len() - MAX_LISTED;
            listing.templates.truncate(MAX_LISTED);
            listing.diagnostics.push(TemplateDiagnostic {
                reference: None,
                path: String::new(),
                error: format!(
                    "{more} more templates were omitted; the listing holds at most {MAX_LISTED}"
                ),
            });
        }
        if listing.diagnostics.len() > MAX_DIAGNOSTICS {
            let more = listing.diagnostics.len() - MAX_DIAGNOSTICS;
            listing.diagnostics.truncate(MAX_DIAGNOSTICS);
            listing.diagnostics.push(TemplateDiagnostic {
                reference: None,
                path: String::new(),
                error: format!("{more} more diagnostics were omitted"),
            });
        }
        listing
    }

    /// The raw file hash and the parsed template for a reference.
    fn template_snapshot(&self, reference: &str) -> Result<(String, Result<Template>)> {
        match TemplateRef::parse(reference)? {
            TemplateRef::User(id) => self.user_templates().snapshot(&id),
            TemplateRef::Pack { pack_id, path } => {
                let discovery = self.discover_packs();
                let (pack, declared) = discovery
                    .pack(&pack_id)
                    .and_then(|pack| {
                        pack.templates
                            .iter()
                            .find(|declared| declared.path == path)
                            .map(|declared| (pack, declared))
                    })
                    .ok_or_else(|| TemplateError::NotFound(reference.to_owned()))?;
                let bytes = pack_file(&pack.canonical_dir, &declared.file)?;
                let template = load(
                    reference.to_owned(),
                    TemplateScope::Pack,
                    Some(pack_id),
                    None,
                    &bytes,
                )
                .map_err(TemplateError::Invalid);
                Ok((sha256_hex(&bytes), template))
            }
        }
    }

    pub fn template(&self, reference: &str) -> Result<Template> {
        self.template_snapshot(reference)?.1
    }

    pub fn create_template(&self, definition: &TemplateDefinition) -> Result<Template> {
        self.user_templates().create(definition)
    }

    pub fn replace_template(
        &self,
        id: &str,
        expected_sha256: &str,
        definition: &TemplateDefinition,
    ) -> Result<Template> {
        self.user_templates()
            .replace(id, expected_sha256, definition)
    }

    pub fn delete_template(&self, id: &str, expected_sha256: &str) -> Result<()> {
        self.user_templates().delete(id, expected_sha256)
    }

    /// Renders the template version the caller reviewed. A different current
    /// version is refused with `Changed`; nothing is rendered from it.
    pub fn render_template(
        &self,
        reference: &str,
        sha256: &str,
        variables: &Map<String, Value>,
    ) -> Result<RenderedTemplate> {
        let (current, template) = self.template_snapshot(reference)?;
        if current != sha256 {
            return Err(TemplateError::Changed {
                current_sha256: current,
            });
        }
        let template = template?;
        Ok(RenderedTemplate {
            text: template.render(variables)?,
            reference: template.reference,
            sha256: template.sha256,
        })
    }
}

#[cfg(test)]
mod tests;
