//! Batch planning, durable creation, the sequential submission driver and
//! recovery. Every item is an ordinary keyed run; the batch only groups them.

use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

use super::*;
use crate::{
    batch::{
        item_submission, item_view, summarise, BatchCounts, BatchItemIssue, BatchItemView,
        BatchListEntry, BatchPlan, BatchRequest, BatchSweep, BatchView, InheritedInput,
        ItemSubmission, PlannedItem, BATCH_PARAMETER_BYTES_CAP, BATCH_PREVIEW_BYTES_CAP,
        MAX_REPORTED_INVALID_ITEMS,
    },
    persistence::{BatchRecord, BatchReservation, NewBatch, NewBatchItem},
};

/// Called with each submitted or replayed run that still needs monitoring.
pub type RunObserver = Arc<dyn Fn(&RunRecord) + Send + Sync>;

/// The outcome of creating or resuming a batch.
pub struct BatchCreation {
    pub batch: BatchView,
    /// The key already belonged to this exact request.
    pub replayed: bool,
    /// Present when items still need submitting. Run it on a tracked task.
    pub driver: Option<BatchDriver>,
}

/// Exclusive right to submit a batch's items, held for the driver's lifetime.
/// The claim is an OS file lock, so a second driver in this or another process
/// cannot start, and a dead process releases it.
pub struct BatchDriver {
    harness: Harness,
    id: String,
    _claim: File,
}

fn preview_too_large(bytes: usize) -> HarnessError {
    HarnessError::BatchTooLarge(format!(
        "The preview would carry at least {bytes} bytes of values, above the limit of {BATCH_PREVIEW_BYTES_CAP}. Shorten the shared values or the workflow defaults, or sweep fewer items."
    ))
}

const INTERRUPTED_BY_RESTART: &str =
    "The server stopped before this item was submitted. Resume the batch to submit it.";

/// The key under which item `index` of a batch is submitted. Batch IDs are
/// short, so this always fits the 128-character key limit.
pub fn batch_item_key(batch_id: &str, index: u32) -> String {
    format!("{batch_id}:{index}")
}

fn claim_name(batch_id: &str) -> String {
    format!("batch:{batch_id}")
}

impl BatchDriver {
    pub fn batch_id(&self) -> &str {
        &self.id
    }

    /// Submits the pending items one at a time. Whatever happens, including a
    /// panic, every item without a run ends as submitted, failed or
    /// `not_submitted`; none is left pending and none is reported successful.
    /// The claim is held, and the work runs, for exactly as long as this future.
    ///
    /// `stop` asks the driver to finish the item in flight and then stop (a
    /// graceful shutdown). `deadline` ends waits on ComfyUI, so an item in
    /// flight then settles as unknown.
    pub async fn run(
        self,
        stop: CancellationToken,
        deadline: CancellationToken,
        observer: RunObserver,
    ) {
        let Self {
            harness,
            id,
            _claim,
        } = self;
        let outcome =
            AssertUnwindSafe(harness.drive_batch(&id, &stop, &deadline, observer.as_ref()))
                .catch_unwind()
                .await;
        let reason = match outcome {
            Ok(Ok(None)) => "The driver ended before this item was attempted.".to_owned(),
            Ok(Ok(Some(reason))) => reason,
            Ok(Err(error)) => format!("The submission driver stopped: {error}"),
            Err(_) => "The submission driver stopped unexpectedly.".to_owned(),
        };
        // A failure here leaves the batch for restart recovery.
        let _ = harness
            .repository
            .as_async()
            .settle_batch(&id, reason)
            .await;
    }

    /// Gives up the claim without submitting anything.
    pub async fn abandon(self, reason: &str) -> Result<()> {
        self.harness
            .repository
            .as_async()
            .settle_batch(&self.id, reason.to_owned())
            .await?;
        Ok(())
    }
}

impl Harness {
    /// Validates a batch against its pinned workflow revision without
    /// submitting anything or touching the backend.
    pub async fn preview_batch(&self, request: &BatchRequest) -> Result<BatchPlan> {
        Ok(self.plan_batch(request).await?.1)
    }

    async fn plan_batch(&self, request: &BatchRequest) -> Result<(LoadedWorkflow, BatchPlan)> {
        request.item_count()?;
        if !single_path_component(&request.workflow) {
            return Err(HarnessError::InvalidBatchRequest(
                "Select a configured workflow by ID, not by path.".into(),
            ));
        }
        let loaded = self
            .load_api_workflow(&request.workflow, Some(&request.workflow_revision))?
            .ok_or_else(|| {
                HarnessError::Workflow(format!("workflow '{}' was not found", request.workflow))
            })?;
        loaded.preset_values(request.preset.as_deref())?;
        let defined = |name: &str| loaded.manifest.parameters.get(name);
        for name in request.sweep_names() {
            if defined(name).is_none() {
                return Err(HarnessError::InvalidBatchRequest(format!(
                    "Sweep parameter '{name}' is not defined by workflow '{}'.",
                    loaded.manifest.id
                )));
            }
        }
        if let BatchSweep::Seeds(sweep) = &request.sweep {
            if defined(&sweep.parameter).map(|definition| definition.kind)
                != Some(ParameterKind::Seed)
            {
                return Err(HarnessError::InvalidBatchRequest(format!(
                    "Parameter '{}' is not a seed parameter; use a grid sweep for other parameters.",
                    sweep.parameter
                )));
            }
        }
        if let Some(name) = request
            .parameters
            .keys()
            .find(|name| defined(name).is_none())
        {
            return Err(HarnessError::InvalidBatchRequest(format!(
                "Parameter '{name}' is not defined by workflow '{}'.",
                loaded.manifest.id
            )));
        }
        let expanded = request.expand()?;
        let mut bytes = 0_usize;
        for (parameters, _) in &expanded {
            bytes = bytes.saturating_add(serde_json::to_vec(parameters)?.len());
        }
        if bytes > BATCH_PARAMETER_BYTES_CAP {
            return Err(HarnessError::BatchTooLarge(format!(
                "The expanded items carry {bytes} bytes of parameters, above the limit of {BATCH_PARAMETER_BYTES_CAP}. Shorten the shared values or sweep fewer items."
            )));
        }
        let mut items = Vec::new();
        let mut inherited_inputs = Vec::new();
        let mut preview_bytes = 0_usize;
        let (mut invalid, mut reported) = (0, Vec::new());
        for (index, (parameters, sweep_values)) in expanded.into_iter().enumerate() {
            let (errors, _) = self
                .check_api_bindings(&loaded, request.preset.as_deref(), &parameters)
                .await?;
            if !errors.is_empty() {
                invalid += 1;
                if reported.len() < MAX_REPORTED_INVALID_ITEMS {
                    reported.push(BatchItemIssue { index, errors });
                }
                continue;
            }
            let effective_parameters = loaded
                .bind(request.preset.as_deref(), &parameters)?
                .parameters;
            for values in [&parameters, &sweep_values, &effective_parameters] {
                preview_bytes = preview_bytes.saturating_add(serde_json::to_vec(values)?.len());
            }
            if preview_bytes > BATCH_PREVIEW_BYTES_CAP {
                return Err(preview_too_large(preview_bytes));
            }
            if index == 0 {
                inherited_inputs = loaded
                    .inherited_inputs(request.preset.as_deref(), &parameters)?
                    .map(|(name, definition, value)| InheritedInput {
                        parameter: name.to_owned(),
                        node: definition.node.clone(),
                        input: definition.input.clone(),
                        value: value.clone(),
                    })
                    .collect();
                for input in &inherited_inputs {
                    preview_bytes =
                        preview_bytes.saturating_add(serde_json::to_vec(&input.value)?.len());
                }
            }
            items.push(PlannedItem {
                index,
                parameters,
                sweep_values,
                effective_parameters,
            });
        }
        if invalid > 0 {
            return Err(HarnessError::BatchItemsInvalid {
                total: invalid,
                items: reported,
            });
        }
        let plan = BatchPlan {
            workflow_revision: loaded.revision(),
            max_items: request.max_items,
            inherited_inputs,
            items,
        };
        Ok((loaded, plan))
    }

    /// Reserves the key and records the batch and all its items atomically.
    /// The caller runs the returned driver. Nothing is submitted if any item
    /// is invalid. Repeating the same key and request replays the batch and
    /// resumes items that have no run yet; another request is a conflict.
    pub async fn create_batch(&self, key: String, request: BatchRequest) -> Result<BatchCreation> {
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&request)?));
        let lock = self
            .events
            .run_lock(&self.database_identity, &key, RunLockKind::BatchKey);
        let _key_guard = lock.lock().await;
        let repository = self.repository.as_async();
        if let Some(existing) = repository
            .find_batch(key.clone(), fingerprint.clone())
            .await?
        {
            return self.resume_existing(existing, true).await;
        }
        let (loaded, plan) = self.plan_batch(&request).await?;
        self.validate_workflow_nodes(&loaded.workflow).await?;
        let id = format!("batch-{}", Uuid::new_v4());
        // Claim before the batch becomes visible, so restart recovery cannot
        // mark it interrupted while this driver is about to start.
        let claim = self
            .try_claim_submission(&claim_name(&id))?
            .ok_or_else(|| HarnessError::Database("new batch already has a driver".into()))?;
        let items = plan
            .items
            .iter()
            .map(|item| {
                Ok(NewBatchItem {
                    parameters: serde_json::to_value(&item.parameters)?,
                })
            })
            .collect::<Result<_>>()?;
        let record = match repository
            .reserve_batch(NewBatch {
                id: id.clone(),
                idempotency_key: key,
                fingerprint,
                workflow_id: loaded.manifest.id.clone(),
                workflow_revision: plan.workflow_revision,
                request: serde_json::to_value(&request)?,
                items,
            })
            .await?
        {
            BatchReservation::New(record) => record,
            BatchReservation::Existing(existing) => {
                drop(claim);
                return self.resume_existing(existing, true).await;
            }
        };
        Ok(BatchCreation {
            batch: self.batch_view(record).await?,
            replayed: false,
            driver: Some(BatchDriver {
                harness: self.clone(),
                id,
                _claim: claim,
            }),
        })
    }

    /// Re-arms items without a run and returns a driver for them, unless one
    /// is already running or nothing is left. Items with a run replay through
    /// their submission key and are never submitted again.
    pub async fn resume_batch(&self, id: &str) -> Result<BatchCreation> {
        let record = self.repository.as_async().get_batch(id).await?;
        self.resume_existing(record, false).await
    }

    async fn resume_existing(&self, record: BatchRecord, replayed: bool) -> Result<BatchCreation> {
        let id = record.id.clone();
        let driver = match self.try_claim_submission(&claim_name(&id))? {
            Some(claim) => {
                let pending = self.repository.as_async().resume_batch(&id).await?;
                (!pending.is_empty()).then(|| BatchDriver {
                    harness: self.clone(),
                    id: id.clone(),
                    _claim: claim,
                })
            }
            None => None,
        };
        let record = self.repository.as_async().get_batch(&id).await?;
        Ok(BatchCreation {
            batch: self.batch_view(record).await?,
            replayed,
            driver,
        })
    }

    pub async fn get_batch(&self, id: &str) -> Result<BatchView> {
        let record = self.repository.as_async().get_batch(id).await?;
        self.batch_view(record).await
    }

    async fn batch_view(&self, record: BatchRecord) -> Result<BatchView> {
        let request: BatchRequest = serde_json::from_value(record.request.clone())?;
        let sweep = request.sweep_names();
        let mut stored = self.repository.as_async().batch_items(&record.id).await?;
        for item in &mut stored {
            // A dead submission owner means the outcome is unknown, not pending.
            if let Some(run) = item.run.take_if(|run| run.status == RunStatus::Submitting) {
                item.run = Some(self.recover_submission(run).await?.0);
            }
        }
        let items = stored
            .into_iter()
            .map(|item| item_view(&sweep, item))
            .collect::<Result<Vec<BatchItemView>>>()?;
        let mut counts = BatchCounts::default();
        items.iter().for_each(|item| counts.add(item.submission));
        Ok(BatchView {
            state: summarise(record.state, items.iter().map(|item| item.submission)),
            record,
            request,
            counts,
            items,
        })
    }

    /// Lists a page of batches with two queries, whatever the page size. It
    /// never recovers submissions: a run whose submission owner died shows as
    /// pending here until the batch is read with `get_batch`.
    pub async fn list_batches(
        &self,
        workflow: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<(Vec<BatchListEntry>, Option<String>)> {
        let repository = self.repository.as_async();
        let page = repository.list_batches(workflow, cursor, limit).await?;
        let ids = page.batches.iter().map(|batch| batch.id.clone()).collect();
        let mut submissions: BTreeMap<String, Vec<ItemSubmission>> = BTreeMap::new();
        for item in repository.batch_item_summaries(ids).await? {
            submissions
                .entry(item.batch_id)
                .or_default()
                .push(item_submission(item.stored, item.run));
        }
        let entries = page
            .batches
            .into_iter()
            .map(|record| {
                let submissions = submissions.remove(&record.id).unwrap_or_default();
                let mut counts = BatchCounts::default();
                submissions.iter().for_each(|item| counts.add(*item));
                BatchListEntry {
                    state: summarise(record.state, submissions),
                    record,
                    counts,
                }
            })
            .collect();
        Ok((entries, page.next_cursor))
    }

    /// Marks batches that a previous process left mid-submission as interrupted,
    /// with their unsent items `not_submitted`. Never resumes them. A batch
    /// whose driver is alive in another process is left alone.
    pub async fn interrupt_unfinished_batches(&self) -> Result<usize> {
        let repository = self.repository.as_async();
        let mut interrupted = 0;
        for id in repository.submitting_batches().await? {
            let Some(_claim) = self.try_claim_submission(&claim_name(&id))? else {
                continue;
            };
            repository
                .settle_batch(&id, INTERRUPTED_BY_RESTART.to_owned())
                .await?;
            interrupted += 1;
        }
        Ok(interrupted)
    }

    /// Submits each pending item once through the keyed run path. Returns the
    /// reason the rest cannot be submitted when a failure affects every item,
    /// such as a changed workflow, a shutdown or an unreachable ComfyUI. A
    /// failure of one item alone, such as a node it needs being missing, fails
    /// only that item.
    async fn drive_batch(
        &self,
        id: &str,
        stop: &CancellationToken,
        deadline: &CancellationToken,
        observer: &(dyn Fn(&RunRecord) + Send + Sync),
    ) -> Result<Option<String>> {
        let repository = self.repository.as_async();
        let record = repository.get_batch(id).await?;
        let request: BatchRequest = serde_json::from_value(record.request)?;
        for (index, parameters) in repository.pending_batch_items(id).await? {
            if stop.is_cancelled() || deadline.is_cancelled() {
                return Ok(Some(
                    "The server began shutting down before this item was submitted.".to_owned(),
                ));
            }
            let run_request = RunRequest {
                workflow: request.workflow.clone(),
                preset: request.preset.clone(),
                parameters: serde_json::from_value(parameters)?,
                version: None,
                workflow_revision: Some(request.workflow_revision.clone()),
                checkpoint_profile: None,
                checkpoint_profile_revision: None,
                character: None,
            };
            let link = BatchLink {
                batch_id: id.to_owned(),
                index,
            };
            match self
                .submit_keyed_request(
                    batch_item_key(id, index),
                    SubmissionRequest::New(run_request),
                    Some(link),
                    deadline,
                )
                .await
            {
                Ok(KeyedSubmission { run, .. }) => {
                    if matches!(
                        run.status,
                        RunStatus::Queued | RunStatus::Running | RunStatus::Cancelling
                    ) {
                        observer(&run);
                    }
                }
                Err(HarnessError::WorkflowChanged) => {
                    return Ok(Some(
                        "The workflow changed after the batch was reviewed, so this item was not submitted."
                            .to_owned(),
                    ))
                }
                Err(
                    error @ (HarnessError::ShuttingDown
                    | HarnessError::WorkflowAmbiguous(_)
                    | HarnessError::WorkflowDiscoveryIncomplete { .. }),
                ) => return Ok(Some(error.to_string())),
                // No prompt was sent, and the next item would meet the same
                // outage, so stop instead of failing every remaining item.
                Err(error @ (HarnessError::ComfyUi(_) | HarnessError::Backend { .. })) => {
                    return Ok(Some(format!(
                        "ComfyUI unreachable; resume when it is back ({error})"
                    )))
                }
                Err(error) => {
                    repository
                        .fail_batch_item(id, index, error.to_string())
                        .await?
                }
            }
        }
        Ok(None)
    }
}
