use std::{
    fs,
    io::{self, Write},
};

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

use super::*;

#[derive(Clone, Debug)]
pub enum SettingSource<'a> {
    Default,
    File(&'a Path),
    Env(&'static str),
}

#[derive(Debug)]
pub struct SettingIssue {
    pub key: String,
    pub reason: String,
}

impl SettingIssue {
    fn new(key: &str, reason: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            reason: reason.into(),
        }
    }

    fn from_validation(error: HarnessError) -> Self {
        match error {
            HarnessError::InvalidSetting { key, reason, .. } => Self { key, reason },
            _ => unreachable!("core field validation returns a structured error"),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Metadata {
    pub(super) target: PathBuf,
    digest: Option<[u8; 32]>,
    file_keys: Vec<String>,
    env: BTreeMap<&'static str, &'static str>,
}

impl Metadata {
    pub(super) fn new(target: PathBuf, source: Option<&str>) -> Self {
        let mut metadata = Self {
            target,
            digest: None,
            file_keys: Vec::new(),
            env: BTreeMap::new(),
        };
        if let Some(source) = source {
            metadata.digest = Some(Sha256::digest(source).into());
            if let Ok(doc) = source.parse::<DocumentMut>() {
                metadata.record_keys(&doc);
            }
        }
        metadata
    }

    fn record_keys(&mut self, doc: &DocumentMut) {
        self.file_keys = doc
            .iter()
            .filter_map(|(section, item)| item.as_table_like().map(|table| (section, table)))
            .flat_map(|(section, table)| {
                table.iter().map(move |(key, _)| format!("{section}.{key}"))
            })
            .collect();
    }

    pub(super) fn mark_env(&mut self, field: &'static str, variable: &'static str) {
        self.env
            .insert(field.strip_prefix("config.").unwrap_or(field), variable);
    }
}

// Explicit wire allow-list: adding a field to core configuration does not expose it.
// A future credential must have separate, write-only handling.
macro_rules! setting_fields {
    ($callback:ident) => {
        $callback! {
            "server.host" => server.host: string,
            "server.port" => server.port: integer,
            "server.max_upload_bytes" => server.max_upload_bytes: integer,
            "comfyui.base_url" => comfyui.base_url: string,
            "comfyui.request_timeout_seconds" => comfyui.request_timeout_seconds: integer,
            "comfyui.websocket_reconnect_seconds" => comfyui.websocket_reconnect_seconds: integer,
            "comfyui.allowed_hosts" => comfyui.allowed_hosts: strings,
            "llm.base_url" => llm.base_url: string,
            "llm.model" => llm.model: string,
            "llm.allowed_hosts" => llm.allowed_hosts: strings,
            "paths.state_database" => state_database: path,
            "paths.workflows" => workflows: path,
            "paths.runs" => runs: path,
            "paths.uploads" => uploads: path,
        }
    };
}

impl ResolvedConfig {
    pub fn settings_file(&self) -> &Path {
        &self.settings.target
    }

    pub fn setting_source(&self, key: &str) -> SettingSource<'_> {
        if let Some(variable) = self.settings.env.get(key) {
            SettingSource::Env(variable)
        } else if self
            .settings
            .file_keys
            .iter()
            .any(|file_key| file_key == key)
        {
            SettingSource::File(self.settings_file())
        } else {
            SettingSource::Default
        }
    }

    pub fn settings_values(&self) -> BTreeMap<&'static str, Value> {
        macro_rules! fields {
            ($($key:literal => $($field:ident).+: $kind:ident,)*) => {
                BTreeMap::from([
                    $(($key, fields!(@json self.$($field).+, $kind)),)*
                    ("claim_legacy_state", json!(self.claim_legacy_state)),
                ])
            };
            (@json $field:expr, path) => { json!($field.to_string_lossy()) };
            (@json $field:expr, $kind:ident) => { json!($field) };
        }
        setting_fields!(fields)
    }

    /// Check and overlay a sparse settings request without reading files or the environment.
    pub fn propose_settings(
        &self,
        patch: BTreeMap<String, Value>,
    ) -> std::result::Result<SettingsUpdate, Vec<SettingIssue>> {
        let current = self.settings_values();
        let mut config = self.clone();
        let mut edits = BTreeMap::new();
        let mut errors = Vec::new();
        for (key, value) in patch {
            let Some(previous) = current.get(key.as_str()) else {
                errors.push(SettingIssue::new(&key, "unknown setting"));
                continue;
            };
            if previous == &value {
                continue;
            }
            if matches!(self.setting_source(&key), SettingSource::Env(_))
                || key == "claim_legacy_state"
            {
                errors.push(SettingIssue::new(&key, "this setting is read-only"));
                continue;
            }
            macro_rules! fields {
                ($($name:literal => $($field:ident).+: $kind:ident,)*) => {
                    match key.as_str() {
                        $($name => set_field!($kind, &mut config.$($field).+),)*
                        _ => unreachable!("unknown and read-only keys have been checked"),
                    }
                };
            }
            macro_rules! set_field {
                (path, $field:expr) => {
                    parse::<String>(&key, value, "a non-empty path string").and_then(|path| {
                        let resolved = resolve_path(
                            &self.workspace,
                            &key,
                            Some(PathBuf::from(&path)),
                            PathBuf::new(),
                        )
                        .map_err(SettingIssue::from_validation)?;
                        *$field = resolved;
                        Ok(TomlValue::from(path))
                    })
                };
                (integer, $field:expr) => {
                    parse(&key, value, "a non-negative integer in range").and_then(|number| {
                        let toml = i64::try_from(number)
                            .map_err(|_| SettingIssue::new(&key, "use an integer in TOML range"))?;
                        *$field = number;
                        Ok(TomlValue::from(toml))
                    })
                };
                (string, $field:expr) => {
                    parse::<String>(&key, value, "a string").map(|text| {
                        *$field = text.clone();
                        TomlValue::from(text)
                    })
                };
                (strings, $field:expr) => {
                    parse::<Vec<String>>(&key, value, "an array of strings").map(|strings| {
                        let array: toml_edit::Array = strings.iter().map(|s| s.as_str()).collect();
                        *$field = strings;
                        TomlValue::from(array)
                    })
                };
            }
            match setting_fields!(fields) {
                Ok(value) => {
                    edits.insert(key, value);
                }
                Err(issue) => errors.push(issue),
            }
        }
        errors.extend(
            validate_fields(&config.server, &config.comfyui, &config.llm)
                .into_iter()
                .map(SettingIssue::from_validation),
        );
        if errors.is_empty() {
            Ok(SettingsUpdate { config, edits })
        } else {
            Err(errors)
        }
    }
}

fn parse<T: DeserializeOwned>(
    key: &str,
    value: Value,
    expected: &str,
) -> std::result::Result<T, SettingIssue> {
    serde_json::from_value(value)
        .map_err(|_| SettingIssue::new(key, format!("expected {expected}")))
}

/// The validated configuration and pending edits to its original config file.
pub struct SettingsUpdate {
    config: ResolvedConfig,
    edits: BTreeMap<String, TomlValue>,
}

impl SettingsUpdate {
    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub fn has_edits(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Reject new non-loopback URLs from the HTTP settings surface. Private/Compose
    /// opt-ins still work via trusted files and environment at startup.
    pub fn validate_http_endpoints(
        &self,
        probe: bool,
    ) -> std::result::Result<(), Vec<SettingIssue>> {
        let mut errors = Vec::new();
        for (key, value, test) in [
            ("comfyui.base_url", &self.config.comfyui.base_url, probe),
            ("llm.base_url", &self.config.llm.base_url, probe),
        ] {
            if !value.is_empty()
                && (test || self.edits.contains_key(key))
                && validate_endpoint(key, value, &[]).is_err()
            {
                errors.push(SettingIssue::new(
                    key,
                    "use a loopback URL for the settings API",
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Check the target and its ancestors, returning whether the database exists.
    pub(crate) fn validate_database_path(&self) -> Result<bool> {
        let target = &self.config.state_database;
        let error = |reason| invalid("paths.state_database", &target.to_string_lossy(), reason);
        let file_reason = "use a regular file or a path to a new database file";
        if matches!(
            target
                .to_string_lossy()
                .rsplit(std::path::is_separator)
                .next(),
            Some("" | "." | "..")
        ) {
            return Err(error(file_reason));
        }
        let ancestors: Vec<_> = target.ancestors().collect();
        let mut exists = false;
        let mut nearest_directory = None;
        for path in ancestors.into_iter().rev() {
            let database = path == target;
            match fs::metadata(path) {
                Ok(metadata)
                    if (database && metadata.is_file()) || (!database && metadata.is_dir()) =>
                {
                    if database {
                        exists = true;
                    } else {
                        nearest_directory = Some(path);
                    }
                }
                // A missing component is fine; a dangling link is not.
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && fs::symlink_metadata(path)
                            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    continue
                }
                _ => {
                    return Err(error(if database {
                        file_reason
                    } else {
                        "use directories for the database path's ancestors"
                    }))
                }
            }
        }
        let directory =
            nearest_directory.ok_or_else(|| error("choose a path under an existing directory"))?;
        if exists {
            check_write_access(target, false)
                .map_err(|_| error("choose a writable database file"))?;
            // SQLite may put WAL sidecars next to the resolved target of a symlink.
            let resolved = target.canonicalize().map_err(|_| error(file_reason))?;
            if let Some(parent) = resolved.parent().filter(|parent| *parent != directory) {
                check_write_access(parent, true).map_err(|_| {
                    error("choose a writable database directory for SQLite sidecar files")
                })?;
            }
        }
        check_write_access(directory, true).map_err(|_| {
            error(if exists {
                "choose a writable database directory for SQLite sidecar files"
            } else {
                "choose a new database path under a writable directory"
            })
        })?;
        Ok(exists)
    }

    /// Atomically replace only the known changed values, refusing a stale source.
    pub fn persist(mut self) -> Result<ResolvedConfig> {
        if self.edits.is_empty() {
            return Ok(self.config);
        }
        let metadata = &mut self.config.settings;
        let path = &metadata.target;
        let expected = metadata.digest;
        let read_current = || -> Result<Option<String>> {
            let current = match fs::symlink_metadata(path) {
                Ok(info) if info.is_file() => Some(fs::read_to_string(path)?),
                Ok(_) => {
                    return Err(HarnessError::SettingsConflict(
                        "The configuration path is no longer a regular file.",
                    ))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if current
                .as_ref()
                .map(|source| <[u8; 32]>::from(Sha256::digest(source)))
                != expected
            {
                return Err(HarnessError::SettingsConflict("The configuration file changed outside the server. Restart to load it before saving."));
            }
            Ok(current)
        };
        let original = read_current()?;
        let mut doc = original.as_deref().unwrap_or_default().parse::<DocumentMut>().map_err(|_| {
            HarnessError::SettingsConflict("The configuration file cannot be edited without losing formatting. Edit it manually and restart.")
        })?;
        for (key, mut value) in self.edits {
            let (section, key) = key
                .split_once('.')
                .expect("editable settings have a section");
            let table = doc
                .entry(section)
                .or_insert(Item::Table(Table::new()))
                .as_table_like_mut()
                .expect("validated config sections are tables");
            if let Some(existing) = table.get_mut(key) {
                if let Some(existing_value) = existing.as_value() {
                    *value.decor_mut() = existing_value.decor().clone();
                }
                *existing = Item::Value(value);
            } else {
                table.insert(key, Item::Value(value));
            }
        }
        let source = doc.to_string();
        // Both TOML implementations must agree before the file is replaced.
        toml::from_str::<HarnessConfig>(&source).map_err(|_| {
            HarnessError::SettingsConflict(
                "The configuration file could not be updated safely. Edit it manually and restart.",
            )
        })?;
        atomic_write(path, source.as_bytes(), || read_current().map(|_| ()))?;
        metadata.digest = Some(Sha256::digest(&source).into());
        metadata.record_keys(&doc);
        Ok(self.config)
    }
}

#[cfg(unix)]
fn check_write_access(path: &Path, directory: bool) -> io::Result<()> {
    use rustix::fs::{accessat, Access, AtFlags, CWD};

    let access = if directory {
        Access::WRITE_OK | Access::EXEC_OK
    } else {
        Access::WRITE_OK
    };
    accessat(CWD, path, access, AtFlags::EACCESS).map_err(io::Error::from)
}

#[cfg(windows)]
fn check_write_access(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    if directory {
        const FILE_ADD_FILE: u32 = 0x0002;
        const FILE_ADD_SUBDIRECTORY: u32 = 0x0004;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options
            .access_mode(FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    } else {
        options.write(true);
    }
    options.open(path).map(drop)
}

#[cfg(not(any(unix, windows)))]
fn check_write_access(path: &Path, _directory: bool) -> io::Result<()> {
    if fs::metadata(path)?.permissions().readonly() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

fn atomic_write(
    path: &Path,
    contents: &[u8],
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .expect("absolute configuration path has a parent");
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    let directory = fs::File::open(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "configuration file is read-only",
            )
            .into());
        }
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    before_rename()?;
    temporary.persist(path).map_err(|error| error.error)?;
    // Once rename succeeds, the new file is authoritative. Some filesystems
    // cannot sync directories; a failure here cannot undo the replacement.
    #[cfg(unix)]
    let _ = directory.sync_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_save_uses_xdg_and_external_changes_are_not_overwritten() {
        let workspace = tempfile::tempdir().unwrap();
        let xdg = workspace.path().join("config");
        let load = || {
            HarnessConfig::load_with(
                workspace.path(),
                None,
                workspace.path(),
                |name| match name {
                    "XDG_CONFIG_HOME" => Some(xdg.to_str().unwrap().to_owned()),
                    "XDG_DATA_HOME" => Some(workspace.path().to_str().unwrap().to_owned()),
                    _ => None,
                },
            )
            .unwrap()
        };
        let initial = load();
        let target = xdg.join("genaimage/genaimage.toml");
        assert_eq!(initial.settings_file(), target);
        assert!(!target.exists());
        let saved = initial
            .propose_settings(BTreeMap::from([("llm.model".into(), json!("local"))]))
            .unwrap()
            .persist()
            .unwrap();
        assert!(
            matches!(saved.setting_source("llm.model"), SettingSource::File(path) if path == target)
        );
        assert_eq!(load().llm.model, "local");

        let externally_edited = "[llm]\nmodel = 'external'\nunknown = true\n";
        fs::write(&target, externally_edited).unwrap();
        let error = saved
            .propose_settings(BTreeMap::from([("llm.model".into(), json!("another"))]))
            .unwrap()
            .persist()
            .unwrap_err();
        assert!(matches!(error, HarnessError::SettingsConflict(_)));
        assert_eq!(fs::read_to_string(&target).unwrap(), externally_edited);
    }

    #[test]
    fn interrupted_write_never_replaces_original() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join(PROJECT_CONFIG);
        fs::write(&path, "# original\n").unwrap();
        let result = atomic_write(&path, b"[server]\nport = 3001\n", || {
            assert_eq!(fs::read_to_string(&path).unwrap(), "# original\n");
            Err(io::Error::other("interrupted before rename").into())
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# original\n");
        assert_eq!(fs::read_dir(workspace.path()).unwrap().count(), 1);
    }
}
