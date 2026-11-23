use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{HarnessError, Result};

const PROJECT_CONFIG: &str = "genaimage.toml";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    pub active_profile: String,
    pub profiles: BTreeMap<String, ComfyUiProfile>,
    pub paths: HarnessPaths,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert("local".to_owned(), ComfyUiProfile::default());

        Self {
            active_profile: "local".to_owned(),
            profiles,
            paths: HarnessPaths::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ComfyUiProfile {
    pub base_url: String,
    pub request_timeout_seconds: u64,
    pub websocket_reconnect_seconds: u64,
}

impl ComfyUiProfile {
    pub fn validate_base_url(&self) -> Result<Url> {
        let base_url = Url::parse(&self.base_url)?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(HarnessError::Configuration(format!(
                "ComfyUI profile URL must use http or https, got '{}'",
                base_url.scheme()
            )));
        }
        if base_url.host_str().is_none() {
            return Err(HarnessError::Configuration(
                "ComfyUI profile URL is missing a host".to_owned(),
            ));
        }
        if !is_loopback_host(base_url.host_str().unwrap_or_default()) {
            return Err(HarnessError::Configuration(format!(
                "this release supports one local ComfyUI host only; '{}' is not loopback",
                base_url.host_str().unwrap_or_default()
            )));
        }
        Ok(base_url)
    }
}

impl Default for ComfyUiProfile {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8188".to_owned(),
            request_timeout_seconds: 60,
            websocket_reconnect_seconds: 2,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct HarnessPaths {
    pub state_database: PathBuf,
    pub workflows: PathBuf,
    pub runs: PathBuf,
    #[serde(rename = "characters", skip_serializing)]
    _retired_path: Option<serde::de::IgnoredAny>,
}

impl Default for HarnessPaths {
    fn default() -> Self {
        Self {
            state_database: PathBuf::from(".genaimage/state.sqlite3"),
            workflows: PathBuf::from("workflows/comfyui"),
            runs: PathBuf::from("runs"),
            _retired_path: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedConfig {
    pub workspace: PathBuf,
    pub active_profile: String,
    pub comfyui: ComfyUiProfile,
    pub state_database: PathBuf,
    pub workflows: PathBuf,
    pub runs: PathBuf,
}

impl HarnessConfig {
    pub fn load(workspace: &Path) -> Result<ResolvedConfig> {
        let workspace = workspace.canonicalize().map_err(|error| {
            HarnessError::Configuration(format!(
                "cannot open workspace '{}': {error}",
                workspace.display()
            ))
        })?;
        let config_path = workspace.join(PROJECT_CONFIG);
        let mut config = if config_path.exists() {
            let source = fs::read_to_string(&config_path)?;
            toml::from_str(&source)?
        } else {
            Self::default()
        };

        if let Ok(profile) = env::var("GENAIMAGE_PROFILE") {
            config.active_profile = profile;
        }
        let profile_name = config.active_profile.clone();
        let profile = config.profiles.get_mut(&profile_name).ok_or_else(|| {
            HarnessError::Configuration(format!("active profile '{profile_name}' is not defined"))
        })?;
        if let Ok(base_url) = env::var("GENAIMAGE_COMFYUI_URL") {
            profile.base_url = base_url;
        }

        let resolve = |path: &Path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.join(path)
            }
        };
        let state_database = env::var_os("GENAIMAGE_STATE_DATABASE")
            .map(PathBuf::from)
            .map_or_else(
                || resolve(&config.paths.state_database),
                |path| resolve(&path),
            );

        let workflows = resolve(&config.paths.workflows);
        let runs = resolve(&config.paths.runs);

        Ok(ResolvedConfig {
            workspace,
            active_profile: profile_name,
            comfyui: profile.clone(),
            state_database,
            workflows,
            runs,
        })
    }

    pub fn example() -> String {
        r#"active_profile = "local"

[profiles.local]
base_url = "http://127.0.0.1:8188"
request_timeout_seconds = 60
websocket_reconnect_seconds = 2

[paths]
state_database = ".genaimage/state.sqlite3"
workflows = "workflows/comfyui"
runs = "runs"
"#
        .to_owned()
    }

    pub fn set_comfyui_url(
        workspace: &Path,
        profile_name: &str,
        base_url: &str,
    ) -> Result<ResolvedConfig> {
        let workspace = workspace.canonicalize().map_err(|error| {
            HarnessError::Configuration(format!(
                "cannot open workspace '{}': {error}",
                workspace.display()
            ))
        })?;
        let config_path = workspace.join(PROJECT_CONFIG);
        let mut config: Self = if config_path.exists() {
            toml::from_str(&fs::read_to_string(&config_path)?)?
        } else {
            Self::default()
        };
        let profile = config.profiles.get_mut(profile_name).ok_or_else(|| {
            HarnessError::Configuration(format!("profile '{profile_name}' is not defined"))
        })?;
        profile.base_url = base_url.to_owned();
        config.active_profile = profile_name.to_owned();
        profile.validate_base_url()?;
        let encoded = toml::to_string_pretty(&config).map_err(|error| {
            HarnessError::Configuration(format!("cannot serialise configuration: {error}"))
        })?;
        let temporary = workspace.join(".genaimage.toml.tmp");
        fs::write(&temporary, encoded)?;
        fs::rename(temporary, config_path)?;
        Self::load(&workspace)
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod tests {
    use super::is_loopback_host;

    #[test]
    fn accepts_only_supported_local_hosts() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("192.168.1.10"));
        assert!(!is_loopback_host("example.com"));
    }
}
