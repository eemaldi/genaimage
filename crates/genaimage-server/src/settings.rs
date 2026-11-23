use std::sync::{Arc, Mutex, RwLock};

use axum::http::StatusCode;
use genaimage_api::{Setting, SettingError, SettingSource, SettingsPatch, SettingsResponse};
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{
    config::{SettingIssue, SettingSource as Source, SettingsUpdate},
    Event, Harness, HarnessError, ResolvedConfig,
};

use crate::Problem;

pub(crate) struct SettingsState {
    snapshot: RwLock<Snapshot>,
    // Serialise saves without blocking readers during file or database I/O.
    writer: Mutex<()>,
}

struct Snapshot {
    desired: Arc<ResolvedConfig>,
    active: Arc<Harness>,
}

impl Snapshot {
    fn response(&self) -> SettingsResponse {
        let active = self.active.config().settings_values();
        let mut restart_required = Vec::new();
        let settings = self
            .desired
            .settings_values()
            .into_iter()
            .map(|(key, value)| {
                let active_value = (active[key] != value).then(|| active[key].clone());
                if active_value.is_some() {
                    restart_required.push(key.to_owned());
                }
                let source = match self.desired.setting_source(key) {
                    Source::Default => SettingSource::Default,
                    Source::File(path) => SettingSource::File {
                        file: path.to_string_lossy().into_owned(),
                    },
                    Source::Env(variable) => SettingSource::Env {
                        variable: variable.into(),
                    },
                };
                let read_only =
                    matches!(source, SettingSource::Env { .. }) || key == "claim_legacy_state";
                (
                    key.to_owned(),
                    Setting {
                        value,
                        source,
                        read_only,
                        active_value,
                    },
                )
            })
            .collect();
        SettingsResponse {
            settings,
            config_file: self.desired.settings_file().to_string_lossy().into_owned(),
            restart_required,
        }
    }
}

impl SettingsState {
    pub(crate) fn new(harness: Arc<Harness>) -> Self {
        Self {
            snapshot: RwLock::new(Snapshot {
                desired: Arc::new(harness.config().clone()),
                active: harness,
            }),
            writer: Mutex::new(()),
        }
    }

    pub(crate) fn harness(&self) -> Arc<Harness> {
        self.snapshot
            .read()
            .expect("settings lock poisoned")
            .active
            .clone()
    }

    pub(crate) fn get(&self) -> SettingsResponse {
        self.snapshot
            .read()
            .expect("settings lock poisoned")
            .response()
    }

    pub(crate) fn propose(
        &self,
        patch: SettingsPatch,
        probe: bool,
    ) -> Result<SettingsUpdate, Problem> {
        let desired = self
            .snapshot
            .read()
            .expect("settings lock poisoned")
            .desired
            .clone();
        let proposal = desired
            .propose_settings(patch.0)
            .map_err(invalid_settings)?;
        proposal
            .validate_http_endpoints(probe)
            .map_err(invalid_settings)?;
        Ok(proposal)
    }

    /// Run the whole transaction on the blocking pool, including publishing the
    /// in-memory snapshot: disconnecting a client cannot interrupt a save.
    pub(crate) fn save(&self, patch: SettingsPatch) -> Result<SettingsResponse, Problem> {
        let _writer = self.writer.lock().expect("settings writer lock poisoned");
        let update = self.propose(patch, false)?;
        if !update.has_edits() {
            return Ok(self.get());
        }
        let active = self.harness();
        let (desired, new_active) = active
            .apply_settings(update, |profile| Ok(Arc::new(ComfyUiClient::new(profile)?)))
            .map_err(|error| match error {
                HarnessError::SettingsConflict(detail) => Problem::new(
                    StatusCode::CONFLICT,
                    detail,
                    "Resolve the configuration conflict and try again.",
                ),
                _ => save_failed(),
            })?;
        let snapshot = Snapshot {
            desired: Arc::new(desired),
            active: Arc::new(new_active),
        };
        let response = snapshot.response();
        *self.snapshot.write().expect("settings lock poisoned") = snapshot;
        active.events().publish(Event::SettingsChanged {
            settings: serde_json::to_value(&response).expect("settings are JSON values"),
        });
        Ok(response)
    }
}

fn invalid_settings(issues: Vec<SettingIssue>) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "One or more settings are invalid.",
        "Correct the listed settings and try again.",
    )
    .with_errors(
        issues
            .into_iter()
            .map(|issue| SettingError {
                key: issue.key,
                reason: issue.reason,
            })
            .collect(),
    )
}

pub(crate) fn save_failed() -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The settings could not be saved.",
        "Check that the configuration file and its directory are writable, then try again.",
    )
}
