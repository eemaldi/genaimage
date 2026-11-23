use std::{
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::{HarnessError, Result};

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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub allowed_hosts: Vec<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: String::new(),
            allowed_hosts: Vec::new(),
        }
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
    pub claim_legacy_state: bool,
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
        let mut config: Self = if let Some(path) = chosen {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let source = fs::read_to_string(&path).map_err(|e| {
                invalid(
                    "config",
                    &path.display().to_string(),
                    &format!("select a readable configuration file ({e})"),
                )
            })?;
            toml::from_str(&source).map_err(|e| HarnessError::Configuration(format!("config '{}' is invalid: {e}; use the documented [server], [comfyui], [llm] and [paths] keys; migrate legacy profiles explicitly (see docs/setup.md)", path.display())))?
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
                    $target = v;
                }
            };
        }
        macro_rules! number {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    $target = v
                        .parse()
                        .map_err(|_| invalid($key, &v, "use a non-negative integer in range"))?;
                }
            };
        }
        macro_rules! path {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
                    $target = Some(PathBuf::from(v));
                }
            };
        }
        macro_rules! hosts {
            ($key:literal, $target:expr) => {
                if let Some(v) = env($key) {
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
        text!("GENAIMAGE_LLM_URL", config.llm.base_url);
        text!("GENAIMAGE_LLM_MODEL", config.llm.model);
        hosts!("GENAIMAGE_LLM_ALLOWED_HOSTS", config.llm.allowed_hosts);
        path!("GENAIMAGE_STATE_DATABASE", config.paths.state_database);
        path!("GENAIMAGE_WORKFLOWS", config.paths.workflows);
        path!("GENAIMAGE_RUNS", config.paths.runs);
        path!("GENAIMAGE_UPLOADS", config.paths.uploads);
        if !is_loopback_name(&config.server.host) {
            return Err(invalid(
                "server.host",
                &config.server.host,
                "bind to a loopback host: localhost, 127.0.0.0/8 or ::1",
            ));
        }
        if config.server.port == 0 {
            return Err(invalid("server.port", "0", "choose a port from 1 to 65535"));
        }
        if config.comfyui.request_timeout_seconds == 0 {
            return Err(invalid(
                "comfyui.request_timeout_seconds",
                "0",
                "use a positive timeout",
            ));
        }
        if config.comfyui.websocket_reconnect_seconds == 0 {
            return Err(invalid(
                "comfyui.websocket_reconnect_seconds",
                "0",
                "use a positive delay",
            ));
        }
        config.comfyui.validate_base_url()?;
        validate_endpoint(
            "llm.base_url",
            &config.llm.base_url,
            &config.llm.allowed_hosts,
        )?;
        let data = xdg("XDG_DATA_HOME", ".local/share")?.join("genaimage");
        let resolve = |key: &str, path: Option<PathBuf>, default: PathBuf| -> Result<PathBuf> {
            let path = path.unwrap_or(default);
            if path.as_os_str().is_empty() {
                return Err(invalid(key, "", "provide a non-empty path"));
            }
            Ok(if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            })
        };
        let claim_legacy_state = match env("GENAIMAGE_CLAIM_LEGACY_STATE").as_deref() {
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
        let workspace_id = hex::encode(Sha256::digest(workspace.as_os_str().as_encoded_bytes()));
        let workspace_data = data.join("workspaces").join(workspace_id);
        Ok(ResolvedConfig {
            claim_legacy_state,
            state_database: resolve(
                "paths.state_database",
                config.paths.state_database,
                workspace_data.join("state.sqlite3"),
            )?,
            workflows: resolve(
                "paths.workflows",
                config.paths.workflows,
                PathBuf::from("workflows/comfyui"),
            )?,
            runs: resolve("paths.runs", config.paths.runs, workspace_data.join("runs"))?,
            uploads: resolve("paths.uploads", config.paths.uploads, data.join("uploads"))?,
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

fn check_legacy_workspace_config(workspace: &Path) -> Result<()> {
    let path = workspace.join(PROJECT_CONFIG);
    let Some(value) = fs::read_to_string(&path)
        .ok()
        .and_then(|source| toml::from_str::<toml::Value>(&source).ok())
    else {
        return Ok(());
    };
    let paths = value.get("paths").and_then(toml::Value::as_table);
    let legacy_paths = paths.is_some_and(|paths| {
        paths.contains_key("characters")
            || ((paths.contains_key("state_database") || paths.contains_key("runs"))
                && !["server", "comfyui", "llm"]
                    .iter()
                    .any(|key| value.get(key).and_then(toml::Value::as_table).is_some()))
    });
    if value.get("active_profile").is_some()
        || value
            .get("profiles")
            .and_then(toml::Value::as_table)
            .is_some()
        || legacy_paths
    {
        return Err(invalid("workspace config", &path.display().to_string(), &format!("legacy workspace configuration is no longer discovered automatically; migrate it to the new format and pass --config '{}', or copy the old state_database and runs paths into the new [paths] section and move the legacy file aside; see docs/setup.md", path.display())));
    }
    Ok(())
}

fn invalid(key: &str, value: &str, fix: &str) -> HarnessError {
    HarnessError::Configuration(format!("{key}='{value}': {fix}"))
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
                "add '{hostname}' to {}.allowed_hosts for a private or Compose host",
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
            "[paths]\nstate_database = 'old.sqlite3'\n",
            "[paths]\nruns = 'old-runs'\n",
            "[comfyui]\n[paths]\ncharacters = 'characters'\n",
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
            "[comfyui]\n[paths]\nruns = 'runs'\n",
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
        fs::write(
            temp.path().join(PROJECT_CONFIG),
            "[server]\nport = 9999\n[llm]\nmodel = 'from-file'\n[paths]\nruns = 'file-runs'\n",
        )
        .unwrap();
        vars.extend([
            ("GENAIMAGE_SERVER_HOST", "::1".into()),
            ("GENAIMAGE_SERVER_PORT", "4242".into()),
            ("GENAIMAGE_COMFYUI_URL", "http://comfyui:8188".into()),
            ("GENAIMAGE_COMFYUI_ALLOWED_HOSTS", "comfyui, other".into()),
            ("GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS", "3".into()),
            ("GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS", "4".into()),
            ("GENAIMAGE_LLM_URL", "http://ollama:11434/v1".into()),
            ("GENAIMAGE_LLM_MODEL", "explicit-model".into()),
            ("GENAIMAGE_LLM_ALLOWED_HOSTS", "ollama".into()),
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
        assert_eq!(config.llm.model, "explicit-model");
        assert_eq!(config.llm.allowed_hosts, ["ollama"]);
        assert_eq!(config.state_database, temp.path().join("db.sqlite"));
        assert_eq!(config.workflows, temp.path().join("wf"));
        assert_eq!(config.runs, temp.path().join("output"));
        assert_eq!(config.uploads, temp.path().join("input"));
        vars.push(("GENAIMAGE_SERVER_PORT", "invalid".into()));
        let error = load(&temp, None, &vars).unwrap_err().to_string();
        assert!(
            error.contains("GENAIMAGE_SERVER_PORT")
                && error.contains("invalid")
                && error.contains("integer")
        );
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
