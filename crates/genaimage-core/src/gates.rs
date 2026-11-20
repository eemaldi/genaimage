use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HarnessError, Result};

pub const STATE_VERSION: u32 = 1;
const MARKER_START: &str = "<!-- BEGIN GENAIMAGE GATE STATE -->";
const MARKER_END: &str = "<!-- END GENAIMAGE GATE STATE -->";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateId {
    Gate1,
    Gate2,
    Gate3,
    Phase04Review,
    Gate4,
    Gate5,
    Phase07Review,
    Phase08Review,
}

impl GateId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gate1 => "Gate 1 - Input completeness",
            Self::Gate2 => "Gate 2 - Design approval",
            Self::Gate3 => "Gate 3 - Brief approval",
            Self::Phase04Review => "Phase 04 - Initial concept review",
            Self::Gate4 => "Gate 4 - Design acceptance",
            Self::Gate5 => "Gate 5 - Visual quality approval",
            Self::Phase07Review => "Phase 07 - Multi-view validation",
            Self::Phase08Review => "Phase 08 - Delivery acceptance",
        }
    }

    pub fn all() -> [Self; 8] {
        [
            Self::Gate1,
            Self::Gate2,
            Self::Gate3,
            Self::Phase04Review,
            Self::Gate4,
            Self::Gate5,
            Self::Phase07Review,
            Self::Phase08Review,
        ]
    }

    pub fn predecessors(self) -> &'static [Self] {
        match self {
            Self::Gate1 => &[],
            Self::Gate2 => &[Self::Gate1],
            Self::Gate3 => &[Self::Gate1, Self::Gate2],
            Self::Phase04Review => &[Self::Gate1, Self::Gate2, Self::Gate3],
            Self::Gate4 => &[Self::Gate1, Self::Gate2, Self::Gate3, Self::Phase04Review],
            Self::Gate5 => &[
                Self::Gate1,
                Self::Gate2,
                Self::Gate3,
                Self::Phase04Review,
                Self::Gate4,
            ],
            Self::Phase07Review => &[
                Self::Gate1,
                Self::Gate2,
                Self::Gate3,
                Self::Phase04Review,
                Self::Gate4,
                Self::Gate5,
            ],
            Self::Phase08Review => &[
                Self::Gate1,
                Self::Gate2,
                Self::Gate3,
                Self::Phase04Review,
                Self::Gate4,
                Self::Gate5,
                Self::Phase07Review,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GateRecord {
    pub status: ApprovalStatus,
    pub decision: Option<String>,
    pub approver: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl Default for GateRecord {
    fn default() -> Self {
        Self {
            status: ApprovalStatus::Pending,
            decision: None,
            approver: None,
            decided_at: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GateDecision {
    pub gate: GateId,
    pub status: ApprovalStatus,
    pub approver: String,
    pub decision: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CharacterState {
    pub schema_version: u32,
    pub character_id: String,
    pub character_name: Option<String>,
    pub current_version: String,
    pub gates: BTreeGateMap,
    #[serde(default)]
    pub decisions: Vec<GateDecision>,
}

pub type BTreeGateMap = std::collections::BTreeMap<GateId, GateRecord>;

impl CharacterState {
    pub fn new(character_id: impl Into<String>, character_name: Option<String>) -> Self {
        let gates = GateId::all()
            .into_iter()
            .map(|gate| (gate, GateRecord::default()))
            .collect();
        Self {
            schema_version: STATE_VERSION,
            character_id: character_id.into(),
            character_name,
            current_version: "0.0".to_owned(),
            gates,
            decisions: Vec::new(),
        }
    }

    pub fn load(character_dir: &Path) -> Result<Self> {
        let path = Self::state_path(character_dir);
        let state: Self = serde_json::from_slice(&fs::read(&path)?)?;
        if state.schema_version != STATE_VERSION {
            return Err(HarnessError::Configuration(format!(
                "unsupported character state schema {} in '{}'",
                state.schema_version,
                path.display()
            )));
        }
        Ok(state)
    }

    pub fn state_path(character_dir: &Path) -> PathBuf {
        character_dir.join("workflow-state.json")
    }

    pub fn decide(
        &mut self,
        gate: GateId,
        status: ApprovalStatus,
        approver: impl Into<String>,
        decision: impl Into<String>,
    ) -> Result<()> {
        let approver = approver.into();
        let decision = decision.into();
        if approver.trim().is_empty() {
            return Err(HarnessError::Gate(
                "an approver is required for every decision".to_owned(),
            ));
        }
        if status == ApprovalStatus::Approved {
            self.assert_predecessors_approved(gate)?;
        }

        let now = Utc::now();
        let record = self.gates.entry(gate).or_default();
        record.status = status;
        record.decision = Some(decision.clone());
        record.approver = Some(approver.clone());
        record.decided_at = Some(now);
        self.decisions.push(GateDecision {
            gate,
            status,
            approver,
            decision,
            decided_at: now,
        });
        if status == ApprovalStatus::Rejected {
            let mut later = false;
            for candidate in GateId::all() {
                if candidate == gate {
                    later = true;
                    continue;
                }
                if later {
                    self.gates.insert(candidate, GateRecord::default());
                }
            }
        }
        Ok(())
    }

    pub fn assert_approved(&self, required: &[GateId]) -> Result<()> {
        for gate in required {
            self.assert_predecessors_approved(*gate)?;
            let record = self.gates.get(gate).ok_or_else(|| {
                HarnessError::Gate(format!(
                    "{} is missing from the character state",
                    gate.label()
                ))
            })?;
            if record.status != ApprovalStatus::Approved {
                return Err(HarnessError::Gate(format!(
                    "{} is {:?}; the operation is blocked until it is explicitly approved",
                    gate.label(),
                    record.status
                )));
            }
        }
        Ok(())
    }

    pub fn save(&self, character_dir: &Path) -> Result<()> {
        fs::create_dir_all(character_dir)?;
        let state_path = Self::state_path(character_dir);
        write_atomic(
            &state_path,
            serde_json::to_vec_pretty(self)
                .map_err(HarnessError::Json)?
                .as_slice(),
        )?;
        self.sync_production_record(character_dir)?;
        Ok(())
    }

    pub fn required_for_generation(&self) -> GateId {
        if self
            .gates
            .get(&GateId::Gate5)
            .is_some_and(|record| record.status == ApprovalStatus::Approved)
        {
            GateId::Gate5
        } else {
            GateId::Gate3
        }
    }

    fn assert_predecessors_approved(&self, gate: GateId) -> Result<()> {
        for predecessor in gate.predecessors() {
            let status = self
                .gates
                .get(predecessor)
                .map_or(ApprovalStatus::Pending, |record| record.status);
            if status != ApprovalStatus::Approved {
                return Err(HarnessError::Gate(format!(
                    "{} cannot be used because {} is {status:?}",
                    gate.label(),
                    predecessor.label()
                )));
            }
        }
        Ok(())
    }

    fn sync_production_record(&self, character_dir: &Path) -> Result<()> {
        let path = character_dir.join("production-record.md");
        let generated = self.generated_markdown();
        let existing = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            format!(
                "# Character Production Record\n\nCharacter: `{}`\n\n",
                self.character_id
            )
        };

        let replacement = if let (Some(start), Some(end)) =
            (existing.find(MARKER_START), existing.find(MARKER_END))
        {
            if end < start {
                return Err(HarnessError::Gate(format!(
                    "production record gate markers are out of order in '{}'",
                    path.display()
                )));
            }
            let end = end + MARKER_END.len();
            format!("{}{}{}", &existing[..start], generated, &existing[end..])
        } else {
            format!("{existing}{generated}\n")
        };
        write_atomic(&path, replacement.as_bytes())
    }

    fn generated_markdown(&self) -> String {
        let mut lines = vec![
            MARKER_START.to_owned(),
            "## Harness-managed gate state".to_owned(),
            String::new(),
            format!("- **Character ID:** `{}`", self.character_id),
            format!("- **Current version:** `{}`", self.current_version),
            String::new(),
            "| Gate | Status | Decision | Approver | Date |".to_owned(),
            "| --- | --- | --- | --- | --- |".to_owned(),
        ];
        for gate in GateId::all() {
            let record = self.gates.get(&gate).cloned().unwrap_or_default();
            let decision = record.decision.unwrap_or_default().replace('|', "\\|");
            let approver = record.approver.unwrap_or_default();
            let date = record
                .decided_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_default();
            lines.push(format!(
                "| {} | {:?} | {} | {} | {} |",
                gate.label(),
                record.status,
                decision,
                approver,
                date
            ));
        }
        lines.push(MARKER_END.to_owned());
        lines.join("\n")
    }
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| HarnessError::InvalidPath {
        path: path.to_path_buf(),
        reason: "file path has no parent directory".to_owned(),
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path)?;
    Ok(())
}
