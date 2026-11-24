use std::{
    collections::BTreeMap,
    env, fmt, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::{HarnessError, Result};

mod settings;
pub use settings::{SettingIssue, SettingSource, SettingsUpdate};

const PROJECT_CONFIG: &str = "genaimage.toml";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HarnessConfig {
    pub server: ServerConfig,
    pub comfyui: ComfyUiProfile,
    pub llm: LlmConfig,
    pub paths: HarnessPaths,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub max_upload_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3000,
            max_upload_bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComfyUiProfile {
    pub base_url: String,
    pub request_timeout_seconds: u64,
    pub websocket_reconnect_seconds: u64,
    pub allowed_hosts: Vec<String>,
}

impl ComfyUiProfile {
    pub fn validate_base_url(&self) -> Result<Url> {
        validate_endpoint("comfyui.base_url", &self.base_url, &self.allowed_hosts)
    }

    /// Canonical ComfyUI base URL used by the adapter and database identity.
    pub fn endpoint_url(&self) -> Result<Url> {
        let mut url = self.validate_base_url()?;
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(url)
    }

    pub fn validate_addresses(&self, addresses: &[SocketAddr]) -> Result<()> {
        validate_endpoint_addresses(
            "comfyui.base_url",
            &self.base_url,
            &self.allowed_hosts,
            addresses,
        )
    }
}

impl Default for ComfyUiProfile {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8188".into(),
            request_timeout_seconds: 60,
            websocket_reconnect_seconds: 2,
            allowed_hosts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmCapabilityOverride {
    pub tools: Option<bool>,
    pub vision: Option<bool>,
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub base_url: String,
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub text_model: String,
    pub vision_model: String,
    pub allowed_hosts: Vec<String>,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub capabilities: BTreeMap<String, LlmCapabilityOverride>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: None,
            text_model: String::new(),
            vision_model: String::new(),
            allowed_hosts: Vec::new(),
            connect_timeout_secs: 5,
            request_timeout_secs: 60,
            idle_timeout_secs: 30,
            capabilities: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for LlmConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("text_model", &self.text_model)
            .field("vision_model", &self.vision_model)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("connect_timeout_secs", &self.connect_timeout_secs)
            .field("request_timeout_secs", &self.request_timeout_secs)
            .field("idle_timeout_secs", &self.idle_timeout_secs)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HarnessPaths {
    pub state_database: Option<PathBuf>,
    pub workflows: Option<PathBuf>,
    pub runs: Option<PathBuf>,
    pub uploads: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedConfig {
    pub workspace: PathBuf,
    pub server: ServerConfig,
    pub comfyui: ComfyUiProfile,
    pub llm: LlmConfig,
    pub state_database: PathBuf,
    pub workflows: PathBuf,
    pub runs: PathBuf,
    pub uploads: PathBuf,
    #[serde(skip)]
    pub(crate) data_dir: PathBuf,
    pub claim_legacy_state: bool,
    #[serde(skip)]
    settings: settings::Metadata,
}

impl HarnessConfig {
    pub fn load(workspace: &Path) -> Result<ResolvedConfig> {
        Self::load_with_config(workspace, None)
    }

    pub fn load_with_config(
        workspace: &Path,
        config_path: Option<&Path>,
    ) -> Result<ResolvedConfig> {
        let cwd = env::current_dir()?;
        Self::load_with(workspace, config_path, &cwd, |key| env::var(key).ok())
    }

    /// The environment lookup and current directory are injected so callers can test discovery safely.
    pub fn load_with(
        workspace: &Path,
        config_path: Option<&Path>,
        cwd: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedConfig> {
        let workspace = workspace.canonicalize().map_err(|error| {
            invalid(
                "workspace",
                &workspace.display().to_string(),
                &format!("choose an existing directory ({error})"),
            )
        })?;
        let home = env("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
        let xdg = |name: &str, fallback: &str| -> Result<PathBuf> {
            if let Some(value) = env(name).filter(|v| !v.is_empty()) {
                let path = PathBuf::from(&value);
                if !path.is_absolute() {
                    return Err(invalid(name, &value, "use an absolute path"));
                }
                return Ok(path);
            }
            home.as_ref()
                .map(|p| p.join(fallback))
                .ok_or_else(|| invalid("HOME", "unset", "set HOME or the relevant XDG directory"))
        };
        let explicit = config_path
            .map(Path::to_path_buf)
            .or_else(|| env("GENAIMAGE_CONFIG").map(PathBuf::from));
        let project = cwd.join(PROJECT_CONFIG);
        let chosen = if let Some(path) = explicit {
            Some(path)
        } else if project.exists() {
            Some(project)
        } else {
            check_legacy_workspace_config(&workspace)?;
            let path = xdg("XDG_CONFIG_HOME", ".config")?
                .join("genaimage")
                .join(PROJECT_CONFIG);
            path.exists().then_some(path)
        };
        let has_file = chosen.is_some();
        let target = match chosen {
            Some(path) => {
                let path = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
                path.canonicalize().map_err(|error| {
                    invalid(
                        "config",
                        &path.display().to_string(),
                        &format!("select a readable configuration file ({error})"),
                    )
                })?
            }
            None => xdg("XDG_CONFIG_HOME", ".config")?
                .join("genaimage")
                .join(PROJECT_CONFIG),
        };
        let source = if has_file {
            Some(fs::read_to_string(&target).map_err(|error| {
                invalid(
                    "config",
                    &target.display().to_string(),
                    &format!("select a readable configuration file ({error})"),
                )
            })?)
        } else {
            None
        };
        let mut settings = settings::Metadata::new(target, source.as_deref());
        if let Some(source) = source.as_deref() {
            if source
                .parse::<toml_edit::DocumentMut>()
                .ok()
                .and_then(|document| document.get("llm")?.as_table_like()?.get("model").cloned())
                .is_some()
            {
                return Err(invalid(
                    "llm.model",
                    "<removed>",
                    "the legacy llm.model key was removed; use llm.text_model and, when needed, llm.vision_model",
                ));
            }
        }
        let mut config: Self = if let Some(source) = source {
            toml::from_str(&source).map_err(|e| HarnessError::Configuration(format!("config '{}' is invalid: {e}; use the documented [server], [comfyui], [llm] and [paths] keys; migrate legacy profiles explicitly (see docs/setup.md)", settings.target.display())))?
        } else {
            Self::default()
        };
        if let Some(value) = env("GENAIMAGE_PROFILE") {
            return Err(invalid(
                "GENAIMAGE_PROFILE",
                &value,
                "remove it and migrate the selected profile to [comfyui]",
            ));
        }
        macro_rules! text {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    settings.mark_env(stringify!($target), $key);
                    $target = v;
                }
            };
        }
        macro_rules! number {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    settings.mark_env(stringify!($target), $key);
                    $target = v
                        .parse()
                        .map_err(|_| invalid($key, &v, "use a non-negative integer in range"))?;
                }
            };
        }
        macro_rules! path {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    settings.mark_env(stringify!($target), $key);
                    $target = Some(PathBuf::from(v));
                }
            };
        }
        macro_rules! hosts {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    settings.mark_env(stringify!($target), $key);
                    $target = if v.trim().is_empty() {
                        Vec::new()
                    } else {
                        v.split(',').map(|s| s.trim().to_owned()).collect()
                    };
                }
            };
        }
        text!("GENAIMAGE_SERVER_HOST", config.server.host);
        number!("GENAIMAGE_SERVER_PORT", config.server.port);
        number!(
            "GENAIMAGE_SERVER_MAX_UPLOAD_BYTES",
            config.server.max_upload_bytes
        );
        text!("GENAIMAGE_COMFYUI_URL", config.comfyui.base_url);
        number!(
            "GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS",
            config.comfyui.request_timeout_seconds
        );
        number!(
            "GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS",
            config.comfyui.websocket_reconnect_seconds
        );
        hosts!(
            "GENAIMAGE_COMFYUI_ALLOWED_HOSTS",
            config.comfyui.allowed_hosts
        );
        if let Some(value) = env("GENAIMAGE_LLM_MODEL") {
            return Err(invalid(
                "GENAIMAGE_LLM_MODEL",
                &value,
                "the legacy variable was removed; use GENAIMAGE_LLM_TEXT_MODEL (and optionally GENAIMAGE_LLM_VISION_MODEL)",
            ));
        }
        text!("GENAIMAGE_LLM_URL", config.llm.base_url);
        if let Some(value) = env("GENAIMAGE_LLM_API_KEY") {
            settings.mark_env("llm.api_key", "GENAIMAGE_LLM_API_KEY");
            config.llm.api_key = (!value.is_empty()).then_some(value);
        }
        text!("GENAIMAGE_LLM_TEXT_MODEL", config.llm.text_model);
        text!("GENAIMAGE_LLM_VISION_MODEL", config.llm.vision_model);
        hosts!("GENAIMAGE_LLM_ALLOWED_HOSTS", config.llm.allowed_hosts);
        number!(
            "GENAIMAGE_LLM_CONNECT_TIMEOUT_SECS",
            config.llm.connect_timeout_secs
        );
        number!(
            "GENAIMAGE_LLM_REQUEST_TIMEOUT_SECS",
            config.llm.request_timeout_secs
        );
        number!(
            "GENAIMAGE_LLM_IDLE_TIMEOUT_SECS",
            config.llm.idle_timeout_secs
        );
        path!("GENAIMAGE_STATE_DATABASE", config.paths.state_database);
        path!("GENAIMAGE_WORKFLOWS", config.paths.workflows);
        path!("GENAIMAGE_RUNS", config.paths.runs);
        path!("GENAIMAGE_UPLOADS", config.paths.uploads);
        if let Some(error) = validate_fields(&config.server, &config.comfyui, &config.llm)
            .into_iter()
            .next()
        {
            return Err(error);
        }
        let data = xdg("XDG_DATA_HOME", ".local/share")?.join("genaimage");
        let claim = env("GENAIMAGE_CLAIM_LEGACY_STATE");
        let claim_legacy_state = match claim.as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(value) => {
                return Err(invalid(
                    "GENAIMAGE_CLAIM_LEGACY_STATE",
                    value,
                    "use 1 to explicitly claim verified legacy records, or unset it",
                ))
            }
        };
        if claim.is_some() {
            settings.mark_env("claim_legacy_state", "GENAIMAGE_CLAIM_LEGACY_STATE");
        }
        let workspace_id = hex::encode(Sha256::digest(workspace.as_os_str().as_encoded_bytes()));
        let workspace_data = data.join("workspaces").join(workspace_id);
        Ok(ResolvedConfig {
            claim_legacy_state,
            settings,
            state_database: resolve_path(
                &workspace,
                "paths.state_database",
                config.paths.state_database,
                workspace_data.join("state.sqlite3"),
            )?,
            workflows: resolve_path(
                &workspace,
                "paths.workflows",
                config.paths.workflows,
                PathBuf::from("workflows/comfyui"),
            )?,
            runs: resolve_path(
                &workspace,
                "paths.runs",
                config.paths.runs,
                workspace_data.join("runs"),
            )?,
            uploads: resolve_path(
                &workspace,
                "paths.uploads",
                config.paths.uploads,
                data.join("uploads"),
            )?,
            data_dir: workspace_data,
            workspace,
            server: config.server,
            comfyui: config.comfyui,
            llm: config.llm,
        })
    }

    pub fn example() -> String {
        include_str!("../../../genaimage.example.toml").to_owned()
    }
}

fn validate_fields(
    server: &ServerConfig,
    comfyui: &ComfyUiProfile,
    llm: &LlmConfig,
) -> Vec<HarnessError> {
    let mut errors = Vec::new();
    if !is_loopback_name(&server.host) {
        errors.push(invalid(
            "server.host",
            &server.host,
            "bind to a loopback host: localhost, 127.0.0.0/8 or ::1",
        ));
    }
    if server.port == 0 {
        errors.push(invalid("server.port", "0", "choose a port from 1 to 65535"));
    }
    if server.max_upload_bytes == 0 {
        errors.push(invalid(
            "server.max_upload_bytes",
            "0",
            "use a positive byte limit",
        ));
    }
    if comfyui.request_timeout_seconds == 0 {
        errors.push(invalid(
            "comfyui.request_timeout_seconds",
            "0",
            "use a positive timeout",
        ));
    }
    if comfyui.websocket_reconnect_seconds == 0 {
        errors.push(invalid(
            "comfyui.websocket_reconnect_seconds",
            "0",
            "use a positive delay",
        ));
    }
    errors.extend(comfyui.validate_base_url().err());
    for (key, value) in [
        ("llm.connect_timeout_secs", llm.connect_timeout_secs),
        ("llm.request_timeout_secs", llm.request_timeout_secs),
        ("llm.idle_timeout_secs", llm.idle_timeout_secs),
    ] {
        if value == 0 {
            errors.push(invalid(key, "0", "use a positive timeout"));
        }
    }
    if !llm.base_url.is_empty() {
        errors.extend(validate_endpoint("llm.base_url", &llm.base_url, &llm.allowed_hosts).err());
    }
    errors
}

fn resolve_path(
    workspace: &Path,
    key: &str,
    path: Option<PathBuf>,
    default: PathBuf,
) -> Result<PathBuf> {
    let path = path.unwrap_or(default);
    if path.as_os_str().is_empty() {
        return Err(invalid(key, "", "provide a non-empty path"));
    }
    Ok(if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    })
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyConfig {
    #[serde(rename = "active_profile")]
    _active_profile: String,
    #[serde(rename = "profiles")]
    _profiles: BTreeMap<String, LegacyComfyUiProfile>,
    #[serde(rename = "paths")]
    _paths: LegacyPaths,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyComfyUiProfile {
    #[serde(rename = "base_url")]
    _base_url: String,
    #[serde(rename = "request_timeout_seconds")]
    _request_timeout_seconds: u64,
    #[serde(rename = "websocket_reconnect_seconds")]
    _websocket_reconnect_seconds: u64,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyPaths {
    #[serde(rename = "state_database")]
    _state_database: PathBuf,
    #[serde(rename = "workflows")]
    _workflows: PathBuf,
    #[serde(rename = "runs")]
    _runs: PathBuf,
    #[serde(rename = "characters")]
    _characters: Option<serde::de::IgnoredAny>,
}

fn check_legacy_workspace_config(workspace: &Path) -> Result<()> {
    let path = workspace.join(PROJECT_CONFIG);
    let Ok(source) = fs::read_to_string(&path) else {
        return Ok(());
    };
    if toml::from_str::<HarnessConfig>(&source).is_ok()
        || toml::from_str::<LegacyConfig>(&source).is_err()
    {
        return Ok(());
    }
    Err(invalid("workspace config", &path.display().to_string(), &format!("legacy workspace configuration is no longer discovered automatically; migrate it to the new format and pass --config '{}', or copy the old state_database and runs paths into the new [paths] section and move the legacy file aside; see docs/setup.md", path.display())))
}

fn invalid(key: &str, value: &str, fix: &str) -> HarnessError {
    HarnessError::InvalidSetting {
        key: key.into(),
        value: value.into(),
        reason: fix.into(),
    }
}

fn is_loopback_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(loopback_ip)
}

fn loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.is_loopback(), |v4| v4.is_loopback()),
        IpAddr::V4(ip) => ip.is_loopback(),
    }
}

fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return local_ip(IpAddr::V4(v4));
            }
            let segments = ip.segments();
            ip.is_loopback() || (segments[0] & 0xfe00 == 0xfc00) || (segments[0] & 0xffc0 == 0xfe80)
        }
    }
}

pub fn validate_endpoint(key: &str, value: &str, allowed_hosts: &[String]) -> Result<Url> {
    let url = Url::parse(value).map_err(|e| {
        invalid(
            key,
            value,
            &format!("provide an http(s) URL with a local host ({e})"),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            key,
            value,
            "use an http(s) URL without credentials, query or fragment",
        ));
    }
    for allowed in allowed_hosts {
        if allowed.is_empty()
            || allowed.contains(['/', ':', '*', ' ', '@']) && allowed.parse::<IpAddr>().is_err()
        {
            return Err(invalid(
                &format!("{}.allowed_hosts", key.split('.').next().unwrap_or(key)),
                allowed,
                "list exact hostnames or IP addresses only",
            ));
        }
    }
    let host = url
        .host()
        .ok_or_else(|| invalid(key, value, "provide a URL with a local host"))?;
    let hostname = match host {
        Host::Domain(s) => s.to_owned(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    if let Ok(ip) = hostname.parse::<IpAddr>() {
        if !local_ip(ip) {
            return Err(invalid(
                key,
                value,
                "public and non-local addresses are not permitted; use loopback or a private host",
            ));
        }
    }
    if !is_loopback_name(&hostname)
        && !allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&hostname))
    {
        return Err(invalid(
            key,
            value,
            &format!(
                "add the hostname to {}.allowed_hosts for a private or Compose host",
                key.split('.').next().unwrap_or(key)
            ),
        ));
    }
    Ok(url)
}

/// Validate every DNS answer before using these exact addresses for connections.
pub fn validate_endpoint_addresses(
    key: &str,
    value: &str,
    allowed_hosts: &[String],
    addresses: &[SocketAddr],
) -> Result<()> {
    let url = validate_endpoint(key, value, allowed_hosts)?;
    let host = match url.host().expect("validated host") {
        Host::Domain(s) => s.to_owned(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    if addresses.is_empty()
        || addresses.iter().any(|address| {
            !local_ip(address.ip()) || (is_loopback_name(&host) && !loopback_ip(address.ip()))
        })
    {
        return Err(invalid(key, value, "ensure every resolved address is private or loopback; public addresses are not permitted"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn load(
        temp: &TempDir,
        explicit: Option<&Path>,
        vars: &[(&str, String)],
    ) -> Result<ResolvedConfig> {
        let values: HashMap<_, _> = vars.iter().cloned().collect();
        HarnessConfig::load_with(temp.path(), explicit, temp.path(), |key| {
            values.get(key).cloned()
        })
    }

    fn values(temp: &TempDir) -> Vec<(&'static str, String)> {
        vec![
            ("HOME", temp.path().display().to_string()),
            (
                "XDG_CONFIG_HOME",
                temp.path().join("config").display().to_string(),
            ),
            (
                "XDG_DATA_HOME",
                temp.path().join("data").display().to_string(),
            ),
        ]
    }

    #[test]
    fn discovery_precedence_and_missing_explicit_file() {
        let temp = TempDir::new().unwrap();
        let xdg = temp.path().join("config/genaimage/genaimage.toml");
        fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        fs::write(&xdg, "[server]\nport = 1001\n").unwrap();
        let project = temp.path().join(PROJECT_CONFIG);
        let env_path = temp.path().join("env.toml");
        let cli_path = temp.path().join("cli.toml");
        let mut env = values(&temp);
        assert_eq!(load(&temp, None, &env).unwrap().server.port, 1001);
        fs::write(&project, "[server]\nport = 1002\n").unwrap();
        assert_eq!(load(&temp, None, &env).unwrap().server.port, 1002);
        fs::write(&env_path, "[server]\nport = 1003\n").unwrap();
        env.push(("GENAIMAGE_CONFIG", env_path.display().to_string()));
        assert_eq!(load(&temp, None, &env).unwrap().server.port, 1003);
        fs::write(&cli_path, "[server]\nport = 1004\n").unwrap();
        assert_eq!(
            load(&temp, Some(&cli_path), &env).unwrap().server.port,
            1004
        );
        let missing = temp.path().join("missing.toml");
        assert!(load(&temp, Some(&missing), &env)
            .unwrap_err()
            .to_string()
            .contains("missing.toml"));
    }

    #[test]
    fn legacy_workspace_probe_runs_only_before_xdg_discovery() {
        let parent = TempDir::new().unwrap();
        let workspace = parent.path().join("project");
        fs::create_dir_all(&workspace).unwrap();
        let workspace_config = workspace.join(PROJECT_CONFIG);
        let mut env: HashMap<_, _> = values(&parent).into_iter().collect();
        let load = |env: &HashMap<&str, String>, explicit: Option<&Path>| {
            HarnessConfig::load_with(&workspace, explicit, parent.path(), |key| {
                env.get(key).cloned()
            })
        };
        let xdg = parent.path().join("config/genaimage/genaimage.toml");
        fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        fs::write(&xdg, "[server]\nport = 1001\n").unwrap();
        for source in [
            "active_profile = 'old'\n",
            "[profiles.old]\nbase_url = 'http://localhost:9000'\n",
            "[paths]\ncharacters = 'characters'\n",
            "[profiles.local]\nbase_url = 'http://localhost:9000'\n[paths]\ncharacters = 'characters'\n",
        ] {
            fs::write(&workspace_config, source).unwrap();
            let error = load(&env, None).unwrap_err().to_string();
            assert!(
                error.contains("project/genaimage.toml")
                    && error.contains("--config")
                    && error.contains("[paths]")
                    && error.contains("no longer discovered"),
                "{source}: {error}"
            );
        }
        for source in [
            "[server\n",
            "[paths]\nworkflows = 'workflows'\n",
            "[paths]\nruns = 'custom-runs'\n",
            "[paths]\nstate_database = 'state.sqlite3'\nruns = 'custom-runs'\n",
            "[comfyui]\n[paths]\ncharacters = 'characters'\n",
            "[comfyui]\n[paths]\nruns = 'runs'\n",
            "active_profile = 123\n",
            "[profiles.local]\nrequest_timeout_seconds = -1\n",
            "[profiles.local]\nunknown = 'value'\n",
        ] {
            fs::write(&workspace_config, source).unwrap();
            assert_eq!(load(&env, None).unwrap().server.port, 1001, "{source}");
        }
        fs::write(&workspace_config, "[paths]\ncharacters = 'characters'\n").unwrap();
        let selected = parent.path().join("other.toml");
        fs::write(&selected, "[server]\nport = 4444\n").unwrap();
        env.insert("GENAIMAGE_CONFIG", selected.display().to_string());
        assert_eq!(load(&env, None).unwrap().server.port, 4444);
        assert_eq!(load(&env, Some(&selected)).unwrap().server.port, 4444);
        fs::write(&workspace_config, "[server\n").unwrap();
        assert_eq!(load(&env, Some(&selected)).unwrap().server.port, 4444);
        assert_eq!(load(&env, None).unwrap().server.port, 4444);
        env.remove("GENAIMAGE_CONFIG");
        fs::write(&workspace_config, "[paths]\nruns = 'old-runs'\n").unwrap();
        fs::write(
            parent.path().join(PROJECT_CONFIG),
            "[server]\nport = 5555\n",
        )
        .unwrap();
        assert_eq!(load(&env, None).unwrap().server.port, 5555);
        fs::write(&workspace_config, "[server\n").unwrap();
        assert_eq!(load(&env, None).unwrap().server.port, 5555);
        assert!(load(&env, Some(&workspace_config)).is_err());
        fs::remove_file(parent.path().join(PROJECT_CONFIG)).unwrap();
        fs::remove_file(&workspace_config).unwrap();
        fs::create_dir(&workspace_config).unwrap();
        assert_eq!(load(&env, None).unwrap().server.port, 1001);
    }

    #[test]
    fn defaults_and_every_environment_override() {
        let temp = TempDir::new().unwrap();
        let mut vars = values(&temp);
        let defaults = load(&temp, None, &vars).unwrap();
        assert_eq!(
            defaults.state_database,
            temp.path()
                .join("data/genaimage/workspaces")
                .join(hex::encode(Sha256::digest(
                    temp.path()
                        .canonicalize()
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                )))
                .join("state.sqlite3")
        );
        assert_eq!(
            defaults.runs,
            defaults.state_database.parent().unwrap().join("runs")
        );
        assert_eq!(defaults.uploads, temp.path().join("data/genaimage/uploads"));
        assert_eq!(defaults.workflows, temp.path().join("workflows/comfyui"));
        assert_eq!(defaults.server.max_upload_bytes, 32 * 1024 * 1024);
        fs::write(
            temp.path().join(PROJECT_CONFIG),
            "[server]\nport = 9999\nmax_upload_bytes = 123\n[llm]\ntext_model = 'from-file'\n[paths]\nruns = 'file-runs'\n",
        )
        .unwrap();
        assert_eq!(
            load(&temp, None, &vars).unwrap().server.max_upload_bytes,
            123
        );
        vars.extend([
            ("GENAIMAGE_SERVER_HOST", "::1".into()),
            ("GENAIMAGE_SERVER_PORT", "4242".into()),
            ("GENAIMAGE_SERVER_MAX_UPLOAD_BYTES", "456".into()),
            ("GENAIMAGE_COMFYUI_URL", "http://comfyui:8188".into()),
            ("GENAIMAGE_COMFYUI_ALLOWED_HOSTS", "comfyui, other".into()),
            ("GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS", "3".into()),
            ("GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS", "4".into()),
            ("GENAIMAGE_LLM_URL", "http://ollama:11434/v1".into()),
            ("GENAIMAGE_LLM_TEXT_MODEL", "explicit-text".into()),
            ("GENAIMAGE_LLM_VISION_MODEL", "explicit-vision".into()),
            ("GENAIMAGE_LLM_API_KEY", "secret-token".into()),
            ("GENAIMAGE_LLM_ALLOWED_HOSTS", "ollama".into()),
            ("GENAIMAGE_LLM_CONNECT_TIMEOUT_SECS", "7".into()),
            ("GENAIMAGE_LLM_REQUEST_TIMEOUT_SECS", "90".into()),
            ("GENAIMAGE_LLM_IDLE_TIMEOUT_SECS", "31".into()),
            ("GENAIMAGE_STATE_DATABASE", "db.sqlite".into()),
            ("GENAIMAGE_WORKFLOWS", "wf".into()),
            ("GENAIMAGE_RUNS", "output".into()),
            ("GENAIMAGE_UPLOADS", "input".into()),
        ]);
        let config = load(&temp, None, &vars).unwrap();
        assert_eq!(
            (config.server.host.as_str(), config.server.port),
            ("::1", 4242)
        );
        assert_eq!(config.server.max_upload_bytes, 456);
        assert_eq!(config.comfyui.base_url, "http://comfyui:8188");
        assert_eq!(config.comfyui.allowed_hosts, ["comfyui", "other"]);
        assert_eq!(
            (
                config.comfyui.request_timeout_seconds,
                config.comfyui.websocket_reconnect_seconds
            ),
            (3, 4)
        );
        assert_eq!(config.llm.base_url, "http://ollama:11434/v1");
        assert_eq!(config.llm.text_model, "explicit-text");
        assert_eq!(config.llm.vision_model, "explicit-vision");
        assert_eq!(config.llm.api_key.as_deref(), Some("secret-token"));
        assert_eq!(config.llm.connect_timeout_secs, 7);
        assert_eq!(config.llm.request_timeout_secs, 90);
        assert_eq!(config.llm.idle_timeout_secs, 31);
        assert_eq!(config.llm.allowed_hosts, ["ollama"]);
        assert_eq!(config.state_database, temp.path().join("db.sqlite"));
        assert_eq!(config.workflows, temp.path().join("wf"));
        assert_eq!(config.runs, temp.path().join("output"));
        assert_eq!(config.uploads, temp.path().join("input"));
        let mut bad_limit = vars.clone();
        bad_limit.push(("GENAIMAGE_SERVER_MAX_UPLOAD_BYTES", "invalid".into()));
        assert!(load(&temp, None, &bad_limit)
            .unwrap_err()
            .to_string()
            .contains("GENAIMAGE_SERVER_MAX_UPLOAD_BYTES"));
        vars.push(("GENAIMAGE_SERVER_PORT", "invalid".into()));
        let error = load(&temp, None, &vars).unwrap_err().to_string();
        assert!(
            error.contains("GENAIMAGE_SERVER_PORT")
                && error.contains("invalid")
                && error.contains("integer")
        );
    }

    #[test]
    fn legacy_model_file_and_environment_are_rejected_actionably() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join(PROJECT_CONFIG),
            "[llm]\nmodel = 'legacy'\n",
        )
        .unwrap();
        let error = load(&temp, None, &values(&temp)).unwrap_err().to_string();
        assert!(error.contains("llm.model"));
        assert!(error.contains("llm.text_model"));
        assert!(error.contains("llm.vision_model"));

        fs::write(temp.path().join(PROJECT_CONFIG), "[llm]\n").unwrap();
        let mut env = values(&temp);
        env.retain(|(key, _)| *key != "GENAIMAGE_LLM_MODEL");
        env.push(("GENAIMAGE_LLM_MODEL", "legacy".into()));
        let legacy = load(&temp, None, &env).unwrap_err().to_string();
        assert!(legacy.contains("GENAIMAGE_LLM_MODEL"));
        assert!(legacy.contains("GENAIMAGE_LLM_TEXT_MODEL"));
    }

    #[test]
    fn parses_role_capability_overrides_and_redacts_api_key() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join(PROJECT_CONFIG),
            "[llm]\nbase_url = 'http://127.0.0.1:11434/v1'\napi_key = 'top-secret'\ntext_model = 'assistant'\nvision_model = 'review'\n[llm.capabilities.assistant]\ntools = true\nvision = false\n",
        )
        .unwrap();
        let config = load(&temp, None, &values(&temp)).unwrap();
        let capabilities = config.llm.capabilities.get("assistant").unwrap();
        assert_eq!(capabilities.tools, Some(true));
        assert_eq!(capabilities.vision, Some(false));
        let debug = format!("{config:?}");
        let json = serde_json::to_string(&config).unwrap();
        assert!(!debug.contains("top-secret"));
        assert!(!json.contains("top-secret"));
        assert!(!config.settings_values().contains_key("llm.api_key"));
    }

    #[test]
    fn validates_all_llm_timeouts() {
        let temp = TempDir::new().unwrap();
        for (variable, field) in [
            (
                "GENAIMAGE_LLM_CONNECT_TIMEOUT_SECS",
                "llm.connect_timeout_secs",
            ),
            (
                "GENAIMAGE_LLM_REQUEST_TIMEOUT_SECS",
                "llm.request_timeout_secs",
            ),
            ("GENAIMAGE_LLM_IDLE_TIMEOUT_SECS", "llm.idle_timeout_secs"),
        ] {
            let mut env = values(&temp);
            env.push((variable, "0".into()));
            let error = load(&temp, None, &env).unwrap_err().to_string();
            assert!(error.contains(field), "{error}");
        }
    }

    #[test]
    fn default_databases_are_scoped_by_canonical_workspace() {
        let parent = TempDir::new().unwrap();
        let first = parent.path().join("one");
        let second = parent.path().join("two");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let vars = values(&parent);
        let env: HashMap<_, _> = vars.into_iter().collect();
        let load = |workspace: &Path| {
            HarnessConfig::load_with(workspace, None, parent.path(), |key| env.get(key).cloned())
                .unwrap()
        };
        let a = load(&first);
        let b = load(&second);
        assert_ne!(a.state_database, b.state_database);
        assert!(a
            .state_database
            .starts_with(parent.path().join("data/genaimage/workspaces")));
        assert_ne!(a.runs, b.runs);
        assert_eq!(a.runs, a.state_database.parent().unwrap().join("runs"));
    }

    #[test]
    fn xdg_falls_back_to_home_and_invalid_paths_fail() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().display().to_string();
        let config = load(&temp, None, &[("HOME", home.clone())]).unwrap();
        assert!(config
            .runs
            .starts_with(temp.path().join(".local/share/genaimage/workspaces")));
        assert_eq!(
            config.runs,
            config.state_database.parent().unwrap().join("runs")
        );
        let error = load(
            &temp,
            None,
            &[("HOME", home), ("XDG_DATA_HOME", "relative".into())],
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("XDG_DATA_HOME")
                && error.contains("relative")
                && error.contains("absolute")
        );
    }

    #[test]
    fn rejects_legacy_and_invalid_server() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(PROJECT_CONFIG);
        fs::write(
            &path,
            "active_profile = 'local'\n[profiles.local]\nbase_url = 'http://localhost:8188'\n",
        )
        .unwrap();
        let error = load(&temp, None, &values(&temp)).unwrap_err().to_string();
        assert!(error.contains("active_profile") && error.contains("migrate"));
        fs::write(&path, "[server]\nmax_upload_bytes = 0\n").unwrap();
        assert!(load(&temp, None, &values(&temp))
            .unwrap_err()
            .to_string()
            .contains("server.max_upload_bytes"));
        fs::write(&path, "[server]\nhost = '0.0.0.0'\n").unwrap();
        let error = load(&temp, None, &values(&temp)).unwrap_err().to_string();
        assert!(
            error.contains("server.host")
                && error.contains("0.0.0.0")
                && error.contains("loopback")
        );
    }

    #[test]
    fn endpoint_policy_rejects_public_and_requires_exact_allowlist() {
        for host in ["localhost", "127.0.0.5", "[::1]", "[::ffff:127.0.0.1]"] {
            assert!(
                validate_endpoint("comfyui.base_url", &format!("http://{host}:8188"), &[]).is_ok()
            );
        }
        for host in ["comfyui", "192.168.1.5", "[fd00::1]"] {
            let url = format!("http://{host}:8188");
            assert!(validate_endpoint("comfyui.base_url", &url, &[]).is_err());
            assert!(validate_endpoint(
                "comfyui.base_url",
                &url,
                &[host.trim_matches(['[', ']']).into()]
            )
            .is_ok());
        }
        for host in ["8.8.8.8", "[2606:4700:4700::1111]", "[::ffff:8.8.8.8]"] {
            let url = format!("http://{host}");
            let error = validate_endpoint(
                "llm.base_url",
                &url,
                &[host.trim_matches(['[', ']']).into()],
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("llm.base_url") && error.contains(&url) && error.contains("public")
            );
        }
        assert!(
            validate_endpoint("llm.base_url", "http://evil.example", &["*.example".into()])
                .is_err()
        );
        assert!(validate_endpoint("llm.base_url", "http://user:pass@localhost", &[]).is_err());
        let local: SocketAddr = "10.0.0.1:8188".parse().unwrap();
        let public: SocketAddr = "8.8.8.8:8188".parse().unwrap();
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://comfyui:8188",
            &["comfyui".into()],
            &[local]
        )
        .is_ok());
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://comfyui:8188",
            &["comfyui".into()],
            &[local, public]
        )
        .is_err());
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://localhost:8188",
            &[],
            &[local]
        )
        .is_err());
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:8188".parse().unwrap();
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://[::ffff:127.0.0.1]:8188",
            &[],
            &[mapped]
        )
        .is_ok());
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://localhost:8188",
            &[],
            &[mapped]
        )
        .is_ok());
        let public_mapped: SocketAddr = "[::ffff:8.8.8.8]:8188".parse().unwrap();
        assert!(validate_endpoint_addresses(
            "comfyui.base_url",
            "http://localhost:8188",
            &[],
            &[public_mapped]
        )
        .is_err());
    }
}
