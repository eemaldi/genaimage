//! Batch requests: explicit seed lists and value grids that expand into
//! ordinary runs. Expansion is deterministic and never uses randomness.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    exact_number::json_values_equal,
    persistence::{BatchItemState, BatchRecord, BatchState, RunRecord, RunStatus},
    workflow::ParameterError,
    HarnessError, Result,
};

/// The server-side ceiling on items per batch, whatever `max_items` says.
pub const BATCH_ITEM_CAP: usize = 256;
/// Bounds the serialised parameters of all items together, so a long shared
/// value cannot multiply into a huge preview, record or response.
pub const BATCH_PARAMETER_BYTES_CAP: usize = 4 * 1024 * 1024;
/// Bounds the invalid items listed in one rejection.
pub const MAX_REPORTED_INVALID_ITEMS: usize = 20;

/// Pinned, fully explicit batch request. Its canonical JSON is fingerprinted.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchRequest {
    pub workflow: String,
    pub workflow_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    pub sweep: BatchSweep,
    pub max_items: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchSweep {
    Seeds(SeedSweep),
    /// Parameter name to the values it takes. Names are sorted and the last
    /// name varies fastest.
    Grid(BTreeMap<String, Vec<Value>>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SeedSweep {
    pub parameter: String,
    pub values: Vec<Value>,
}

/// An item's run parameters and, separately, its swept values.
pub type ExpandedItem = (BTreeMap<String, Value>, BTreeMap<String, Value>);

/// A rejected item and every parameter problem found in it.
#[derive(Clone, Debug)]
pub struct BatchItemIssue {
    pub index: usize,
    pub errors: Vec<ParameterError>,
}

#[derive(Clone, Debug)]
pub struct InheritedInput {
    pub parameter: String,
    pub node: String,
    pub input: String,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct PlannedItem {
    pub index: usize,
    /// Exactly what the item's run request carries: base values plus sweep values.
    pub parameters: BTreeMap<String, Value>,
    pub sweep_values: BTreeMap<String, Value>,
    /// Preset, explicit values and workflow defaults, as the run will record them.
    pub effective_parameters: BTreeMap<String, Value>,
    pub inherited_inputs: Vec<InheritedInput>,
}

#[derive(Clone, Debug)]
pub struct BatchPlan {
    pub workflow_revision: String,
    pub max_items: usize,
    pub items: Vec<PlannedItem>,
}

impl BatchSweep {
    /// The swept parameters with their values, in expansion order.
    pub(crate) fn axes(&self) -> Vec<(&str, &[Value])> {
        match self {
            Self::Seeds(sweep) => vec![(sweep.parameter.as_str(), sweep.values.as_slice())],
            Self::Grid(grid) => grid
                .iter()
                .map(|(name, values)| (name.as_str(), values.as_slice()))
                .collect(),
        }
    }
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::InvalidBatchRequest(message.into())
}

impl BatchRequest {
    /// Checks the request without a workflow and returns its item count.
    pub fn item_count(&self) -> Result<usize> {
        if self.max_items == 0 {
            return Err(invalid("max_items must be at least 1."));
        }
        let axes = self.sweep.axes();
        if axes.is_empty() {
            return Err(invalid("A grid sweep needs at least one parameter."));
        }
        let mut count = 1_usize;
        for (name, values) in &axes {
            if values.is_empty() {
                return Err(invalid(format!("Sweep parameter '{name}' has no values.")));
            }
            if self.parameters.contains_key(*name) {
                return Err(invalid(format!(
                    "Sweep parameter '{name}' is also set in parameters; set it in only one place."
                )));
            }
            count = count.saturating_mul(values.len());
        }
        if count > self.max_items {
            return Err(HarnessError::BatchTooLarge(format!(
                "The sweep expands to {count} items, above max_items {}. Nothing was truncated or submitted.",
                self.max_items
            )));
        }
        if count > BATCH_ITEM_CAP {
            return Err(HarnessError::BatchTooLarge(format!(
                "The sweep expands to {count} items, above the server limit of {BATCH_ITEM_CAP}. Nothing was truncated or submitted."
            )));
        }
        for (name, values) in &axes {
            for (later, value) in values.iter().enumerate().skip(1) {
                for (earlier, other) in values[..later].iter().enumerate() {
                    let same = json_values_equal(value, other).map_err(|()| {
                        invalid(format!(
                            "Sweep parameter '{name}' has a number that cannot be compared exactly."
                        ))
                    })?;
                    if same {
                        return Err(invalid(format!(
                            "Sweep parameter '{name}' repeats a value at positions {earlier} and {later}."
                        )));
                    }
                }
            }
        }
        Ok(count)
    }

    /// Expands the sweep. Item `i` takes, for each axis in sorted name order,
    /// the value at its digit of `i` in a mixed-radix number whose last axis
    /// is the least significant digit.
    pub fn expand(&self) -> Result<Vec<ExpandedItem>> {
        let count = self.item_count()?;
        let axes = self.sweep.axes();
        Ok((0..count)
            .map(|index| {
                let mut remainder = index;
                let mut sweep_values = BTreeMap::new();
                for (name, values) in axes.iter().rev() {
                    sweep_values
                        .insert((*name).to_owned(), values[remainder % values.len()].clone());
                    remainder /= values.len();
                }
                let mut parameters = self.parameters.clone();
                parameters.extend(sweep_values.clone());
                (parameters, sweep_values)
            })
            .collect())
    }

    pub fn sweep_names(&self) -> Vec<&str> {
        self.sweep
            .axes()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }
}

/// Whether the driver has submitted an item, as shown to clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemSubmission {
    Pending,
    Submitted,
    Failed,
    NotSubmitted,
    /// ComfyUI may have accepted the prompt. Reconcile the run; never resubmit.
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchSummary {
    Submitting,
    Interrupted,
    /// Every item has a run that ComfyUI accepted. Run outcomes are separate.
    Submitted,
    /// Some items were not accepted or are unknown, and none are pending.
    Partial,
    /// No item was accepted.
    Failed,
}

pub(crate) fn item_submission(
    stored: BatchItemState,
    run: Option<(RunStatus, bool)>,
) -> ItemSubmission {
    match run {
        Some((RunStatus::Submitting, _)) => ItemSubmission::Pending,
        Some((RunStatus::UnknownSubmission, _)) => ItemSubmission::OutcomeUnknown,
        // A run that failed before preflight never reached the backend.
        Some((RunStatus::Failed, true)) => ItemSubmission::Failed,
        Some(_) => ItemSubmission::Submitted,
        None => match stored {
            BatchItemState::Pending => ItemSubmission::Pending,
            BatchItemState::Failed => ItemSubmission::Failed,
            BatchItemState::NotSubmitted => ItemSubmission::NotSubmitted,
        },
    }
}

pub(crate) fn summarise(
    state: BatchState,
    items: impl IntoIterator<Item = ItemSubmission>,
) -> BatchSummary {
    match state {
        BatchState::Submitting => return BatchSummary::Submitting,
        BatchState::Interrupted => return BatchSummary::Interrupted,
        BatchState::Finished => {}
    }
    let mut counts = BatchCounts::default();
    items.into_iter().for_each(|item| counts.add(item));
    if counts.pending > 0 {
        BatchSummary::Submitting
    } else if counts.not_submitted > 0 {
        BatchSummary::Interrupted
    } else if counts.submitted == 0 && counts.outcome_unknown == 0 {
        BatchSummary::Failed
    } else if counts.failed > 0 || counts.outcome_unknown > 0 {
        BatchSummary::Partial
    } else {
        BatchSummary::Submitted
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BatchCounts {
    pub pending: usize,
    pub submitted: usize,
    pub failed: usize,
    pub not_submitted: usize,
    pub outcome_unknown: usize,
}

impl BatchCounts {
    pub(crate) fn add(&mut self, item: ItemSubmission) {
        match item {
            ItemSubmission::Pending => self.pending += 1,
            ItemSubmission::Submitted => self.submitted += 1,
            ItemSubmission::Failed => self.failed += 1,
            ItemSubmission::NotSubmitted => self.not_submitted += 1,
            ItemSubmission::OutcomeUnknown => self.outcome_unknown += 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatchItemView {
    pub index: u32,
    pub parameters: BTreeMap<String, Value>,
    pub sweep_values: BTreeMap<String, Value>,
    pub run_id: Option<String>,
    pub submission: ItemSubmission,
    pub error: Option<String>,
    pub run_status: Option<RunStatus>,
}

#[derive(Clone, Debug)]
pub struct BatchView {
    pub record: BatchRecord,
    pub request: BatchRequest,
    pub state: BatchSummary,
    pub counts: BatchCounts,
    pub items: Vec<BatchItemView>,
}

#[derive(Clone, Debug)]
pub struct BatchListEntry {
    pub record: BatchRecord,
    pub state: BatchSummary,
    pub counts: BatchCounts,
}

pub(crate) fn item_view(
    sweep: &[&str],
    stored: crate::persistence::StoredBatchItem,
) -> Result<BatchItemView> {
    let parameters: BTreeMap<String, Value> = serde_json::from_value(stored.parameters)?;
    let sweep_values = sweep
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), parameters.get(*name)?.clone())))
        .collect();
    let run: Option<&RunRecord> = stored.run.as_ref();
    let submission = item_submission(
        stored.state,
        run.map(|run| (run.status, run.preflight_sha256.is_none())),
    );
    let error = match (submission, run) {
        (ItemSubmission::Failed | ItemSubmission::OutcomeUnknown, Some(run)) => run.error.clone(),
        _ => stored.error,
    };
    Ok(BatchItemView {
        index: stored.index,
        parameters,
        sweep_values,
        run_id: run.map(|run| run.id.clone()),
        submission,
        error,
        run_status: run.map(|run| run.status),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn grid(base: Value, grid: Value, max_items: usize) -> BatchRequest {
        serde_json::from_value(json!({
            "workflow": "w", "workflow_revision": "r", "parameters": base,
            "sweep": {"grid": grid}, "max_items": max_items
        }))
        .unwrap()
    }

    #[test]
    fn grid_expands_in_sorted_name_order_with_the_last_name_fastest() {
        let request = grid(
            json!({"steps": 20}),
            json!({"seed": [1, 2], "cfg": [7.0, 7.5, 8], "alpha": ["a", "b"]}),
            12,
        );
        let items = request.expand().unwrap();
        let rows: Vec<String> = items
            .iter()
            .map(|(parameters, _)| {
                format!(
                    "{} {} {} {}",
                    parameters["alpha"], parameters["cfg"], parameters["seed"], parameters["steps"]
                )
            })
            .collect();
        // Names sort to alpha, cfg, seed: seed varies fastest, then cfg, then alpha.
        assert_eq!(
            rows,
            [
                "\"a\" 7.0 1 20",
                "\"a\" 7.0 2 20",
                "\"a\" 7.5 1 20",
                "\"a\" 7.5 2 20",
                "\"a\" 8 1 20",
                "\"a\" 8 2 20",
                "\"b\" 7.0 1 20",
                "\"b\" 7.0 2 20",
                "\"b\" 7.5 1 20",
                "\"b\" 7.5 2 20",
                "\"b\" 8 1 20",
                "\"b\" 8 2 20",
            ]
        );
        assert_eq!(
            items[5].1,
            BTreeMap::from([
                ("alpha".to_owned(), json!("a")),
                ("cfg".to_owned(), json!(8)),
                ("seed".to_owned(), json!(2)),
            ])
        );
    }

    #[test]
    fn seed_lists_keep_large_seeds_and_number_spelling_exactly() {
        let request: BatchRequest = serde_json::from_str(
            r#"{"workflow":"w","workflow_revision":"r","parameters":{"cfg":2.0},
                "sweep":{"seeds":{"parameter":"seed","values":[18446744073709551615,5,-0.0]}},
                "max_items":3}"#,
        )
        .unwrap();
        let canonical = serde_json::to_string(&request.expand().unwrap()[0].0).unwrap();
        assert_eq!(canonical, r#"{"cfg":2.0,"seed":18446744073709551615}"#);
        let canonical = serde_json::to_string(&request.expand().unwrap()[2].0).unwrap();
        assert_eq!(canonical, r#"{"cfg":2.0,"seed":-0.0}"#);
    }

    #[test]
    fn size_limits_reject_instead_of_truncating() {
        let request = grid(json!({}), json!({"a": [1, 2, 3], "b": [1, 2]}), 5);
        assert!(matches!(
            request.expand(),
            Err(HarnessError::BatchTooLarge(message)) if message.contains("6 items") && message.contains("max_items 5")
        ));
        let values: Vec<u32> = (0..BATCH_ITEM_CAP as u32 + 1).collect();
        let request = grid(json!({}), json!({ "a": values }), 10_000);
        assert!(matches!(
            request.expand(),
            Err(HarnessError::BatchTooLarge(message)) if message.contains("server limit of 256")
        ));
        let request = grid(json!({}), json!({"a": [1]}), 0);
        assert!(matches!(
            request.expand(),
            Err(HarnessError::InvalidBatchRequest(_))
        ));
    }

    #[test]
    fn overlap_duplicates_and_empty_axes_are_rejected() {
        for (base, sweep, needle) in [
            (
                json!({"seed": 1}),
                json!({"seed": [1, 2]}),
                "also set in parameters",
            ),
            (json!({}), json!({"seed": [1, 2, 1]}), "positions 0 and 2"),
            (json!({}), json!({"cfg": [2, 2.0]}), "positions 0 and 1"),
            (json!({}), json!({"seed": []}), "no values"),
            (json!({}), json!({}), "at least one parameter"),
        ] {
            let error = grid(base, sweep, 10).expand().unwrap_err();
            assert!(
                matches!(&error, HarnessError::InvalidBatchRequest(message) if message.contains(needle)),
                "{error}"
            );
        }
    }

    #[test]
    fn requests_reject_unknown_fields_and_ambiguous_sweeps() {
        for body in [
            r#"{"workflow":"w","workflow_revision":"r","sweep":{"grid":{"a":[1]}}}"#,
            r#"{"workflow":"w","sweep":{"grid":{"a":[1]}},"max_items":1}"#,
            r#"{"workflow":"w","workflow_revision":"r","max_items":1,"sweep":{"grid":{"a":[1]},"seeds":{"parameter":"s","values":[1]}}}"#,
            r#"{"workflow":"w","workflow_revision":"r","max_items":1,"sweep":{"grid":{"a":[1]}},"extra":1}"#,
        ] {
            assert!(
                serde_json::from_str::<BatchRequest>(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn summaries_never_report_success_for_unknown_or_missing_items() {
        use ItemSubmission::*;
        let state =
            |items: &[ItemSubmission]| summarise(BatchState::Finished, items.iter().copied());
        assert_eq!(state(&[Submitted, Submitted]), BatchSummary::Submitted);
        assert_eq!(state(&[Submitted, OutcomeUnknown]), BatchSummary::Partial);
        assert_eq!(state(&[Submitted, Failed]), BatchSummary::Partial);
        assert_eq!(state(&[Failed, Failed]), BatchSummary::Failed);
        assert_eq!(state(&[Submitted, Pending]), BatchSummary::Submitting);
        assert_eq!(state(&[Submitted, NotSubmitted]), BatchSummary::Interrupted);
        assert_eq!(
            summarise(BatchState::Interrupted, [Submitted]),
            BatchSummary::Interrupted
        );
    }
}
