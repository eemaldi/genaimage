//! Offline first-run assessment: does this installation still need the setup wizard?

use crate::{
    config::{ResolvedConfig, SettingSource},
    HarnessError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupReasonCode {
    /// No configuration file exists on disk.
    NoConfigFile,
    /// An endpoint in the active configuration fails validation.
    InvalidConfig,
    /// The environment supplies the ComfyUI endpoint, as in Compose.
    EndpointsFromEnvironment,
    /// No LLM endpoint is configured; the assistant is optional.
    LlmNotConfigured,
    /// An LLM endpoint is configured but no text model is chosen.
    LlmTextModelUnset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupReason {
    pub code: SetupReasonCode,
    pub summary: String,
    /// Only blocking reasons make `needs_setup` true.
    pub blocking: bool,
    pub fix: Option<String>,
    pub items: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupAssessment {
    pub needs_setup: bool,
    pub reasons: Vec<SetupReason>,
    pub config_file_exists: bool,
    pub endpoints_env_owned: bool,
}

fn reason(code: SetupReasonCode, blocking: bool, summary: &str, fix: Option<&str>) -> SetupReason {
    SetupReason {
        code,
        summary: summary.into(),
        blocking,
        fix: fix.map(Into::into),
        items: Vec::new(),
    }
}

/// Judge the saved configuration without any network access.
pub fn assess(config: &ResolvedConfig) -> SetupAssessment {
    let config_file_exists = config.settings_file().is_file();
    let endpoints_env_owned = matches!(
        config.setting_source("comfyui.base_url"),
        SettingSource::Env(_)
    );
    let mut reasons = Vec::new();
    if !config_file_exists {
        reasons.push(reason(
            SetupReasonCode::NoConfigFile,
            !endpoints_env_owned,
            "No configuration file exists yet.",
            Some("Run the setup wizard to save one, or `genaimage config init`."),
        ));
    }
    let invalid = config.endpoint_errors();
    if !invalid.is_empty() {
        let mut found = reason(
            SetupReasonCode::InvalidConfig,
            true,
            "The configuration contains invalid endpoints.",
            Some("Correct the listed settings in the wizard or on the Settings page."),
        );
        found.items = invalid
            .iter()
            .map(|error| match error {
                HarnessError::InvalidSetting { key, reason, .. } => format!("{key}: {reason}"),
                other => other.to_string(),
            })
            .collect();
        reasons.push(found);
    }
    if endpoints_env_owned {
        reasons.push(reason(
            SetupReasonCode::EndpointsFromEnvironment,
            false,
            "The environment supplies the ComfyUI endpoint, so it cannot be changed here.",
            None,
        ));
    }
    if config.llm.base_url.is_empty() {
        reasons.push(reason(
            SetupReasonCode::LlmNotConfigured,
            false,
            "No LLM endpoint is configured; the assistant is disabled.",
            Some("Optional: choose an OpenAI-compatible endpoint in the wizard."),
        ));
    } else if config.llm.text_model.is_empty() {
        reasons.push(reason(
            SetupReasonCode::LlmTextModelUnset,
            false,
            "An LLM endpoint is configured but no text model is chosen.",
            Some("Choose a text model in the wizard or set llm.text_model."),
        ));
    }
    SetupAssessment {
        needs_setup: reasons.iter().any(|found| found.blocking),
        reasons,
        config_file_exists,
        endpoints_env_owned,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs};

    use tempfile::TempDir;

    use super::*;
    use crate::HarnessConfig;

    fn assess_with(file: Option<&str>, env: &[(&str, &str)]) -> SetupAssessment {
        let temp = TempDir::new().unwrap();
        let root = temp.path().display().to_string();
        let mut vars: HashMap<String, String> = ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"]
            .into_iter()
            .map(|name| (name.to_owned(), root.clone()))
            .collect();
        vars.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let path = temp.path().join("chosen.toml");
        if let Some(file) = file {
            fs::write(&path, file).unwrap();
        } else {
            // A missing explicit file is an error; select the default location instead.
            vars.remove("GENAIMAGE_CONFIG");
        }
        let explicit = file.is_some().then_some(path.as_path());
        let config = HarnessConfig::load_with(temp.path(), explicit, temp.path(), |key| {
            vars.get(key).cloned()
        })
        .unwrap();
        assess(&config)
    }

    fn codes(assessment: &SetupAssessment) -> Vec<SetupReasonCode> {
        assessment.reasons.iter().map(|found| found.code).collect()
    }

    #[test]
    fn no_file_and_no_environment_needs_setup() {
        let found = assess_with(None, &[]);
        assert!(found.needs_setup);
        assert!(!found.config_file_exists);
        assert!(!found.endpoints_env_owned);
        assert_eq!(codes(&found)[0], SetupReasonCode::NoConfigFile);
        assert!(found.reasons[0].blocking);
    }

    #[test]
    fn environment_owned_endpoints_do_not_need_setup() {
        let found = assess_with(
            None,
            &[
                ("GENAIMAGE_COMFYUI_URL", "http://comfyui:8188"),
                ("GENAIMAGE_COMFYUI_ALLOWED_HOSTS", "comfyui"),
                ("GENAIMAGE_LLM_URL", "http://127.0.0.1:11434/v1"),
            ],
        );
        assert!(!found.needs_setup, "{found:?}");
        assert!(found.endpoints_env_owned);
        assert!(!found.config_file_exists);
        assert_eq!(
            codes(&found),
            [
                SetupReasonCode::NoConfigFile,
                SetupReasonCode::EndpointsFromEnvironment,
                SetupReasonCode::LlmTextModelUnset
            ]
        );
        assert!(found.reasons.iter().all(|found| !found.blocking));
    }

    #[test]
    fn valid_file_needs_no_setup() {
        let found = assess_with(
            Some("[comfyui]\nbase_url = \"http://127.0.0.1:8188\"\n"),
            &[],
        );
        assert!(!found.needs_setup);
        assert!(found.config_file_exists);
        assert_eq!(codes(&found), [SetupReasonCode::LlmNotConfigured]);
    }

    #[test]
    fn invalid_endpoint_needs_setup_even_with_a_file() {
        // Loading rejects invalid endpoints, so corrupt a loaded configuration directly.
        let temp = TempDir::new().unwrap();
        let root = temp.path().display().to_string();
        let path = temp.path().join("chosen.toml");
        fs::write(&path, "[comfyui]\nbase_url = \"http://127.0.0.1:8188\"\n").unwrap();
        let mut config = HarnessConfig::load_with(temp.path(), Some(&path), temp.path(), |key| {
            matches!(key, "HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME").then(|| root.clone())
        })
        .unwrap();
        config.comfyui.base_url = "http://comfyui:8188".into();
        let found = assess(&config);
        assert!(found.needs_setup && found.config_file_exists);
        let invalid = found
            .reasons
            .iter()
            .find(|found| found.code == SetupReasonCode::InvalidConfig)
            .unwrap();
        assert!(invalid.blocking);
        assert!(
            invalid.items[0].contains("comfyui.allowed_hosts"),
            "{invalid:?}"
        );
    }
}
