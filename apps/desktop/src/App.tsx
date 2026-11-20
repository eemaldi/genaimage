import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "./api";
import type {
  CharacterState,
  GateId,
  JsonValue,
  ResolvedConfig,
  RunDetails,
  RunProgress,
  RunRecord,
  WorkflowInspection,
  WorkflowManifest
} from "./types";
import {
  formatBytes,
  formValue,
  isTrackable,
  parseParameterValue,
  shortHash,
  statusLabel
} from "./utils";

type Section = "overview" | "launch" | "jobs" | "character" | "settings";

const gateLabels: Record<GateId, string> = {
  gate1: "Gate 1 — Input completeness",
  gate2: "Gate 2 — Design approval",
  gate3: "Gate 3 — Brief approval",
  phase04_review: "Phase 04 — Initial concept review",
  gate4: "Gate 4 — Design acceptance",
  gate5: "Gate 5 — Visual quality approval",
  phase07_review: "Phase 07 — Multi-view validation",
  phase08_review: "Phase 08 — Delivery acceptance"
};
const gateOrder = Object.keys(gateLabels) as GateId[];

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function App() {
  const [section, setSection] = useState<Section>("overview");
  const [workspace, setWorkspace] = useState("");
  const [config, setConfig] = useState<ResolvedConfig | null>(null);
  const [workflows, setWorkflows] = useState<WorkflowManifest[]>([]);
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [healthy, setHealthy] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  const [refreshing, setRefreshing] = useState(false);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [workspaceValue, configResult, workflowResult, runResult] = await Promise.all([
        api.currentWorkspace(),
        api.configuration(),
        api.listWorkflows(),
        api.listRuns()
      ]);
      setWorkspace(workspaceValue);
      setConfig(configResult);
      setWorkflows(workflowResult);
      setRuns(runResult);
      setError("");
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setRefreshing(false);
    }
  }, []);

  const checkHealth = useCallback(async () => {
    setHealthy(null);
    try {
      await api.health();
      setHealthy(true);
    } catch (caught) {
      setHealthy(false);
      setError(errorMessage(caught));
    }
  }, []);

  useEffect(() => {
    void refresh();
    void checkHealth();
    let unlistenProgress: (() => void) | undefined;
    let unlistenFinished: (() => void) | undefined;
    void listen<RunProgress>("genaimage://run-progress", (event) => {
      const updated = event.payload.run;
      setRuns((current) => [updated, ...current.filter((run) => run.id !== updated.id)]);
    }).then((unlisten) => {
      unlistenProgress = unlisten;
    });
    void listen<RunRecord>("genaimage://run-finished", (event) => {
      const updated = event.payload;
      setRuns((current) => [updated, ...current.filter((run) => run.id !== updated.id)]);
    }).then((unlisten) => {
      unlistenFinished = unlisten;
    });
    return () => {
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, [checkHealth, refresh]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      void api.listRuns().then(setRuns).catch((caught) => setError(errorMessage(caught)));
    }, 4000);
    return () => window.clearInterval(timer);
  }, []);

  const selectedNav = useMemo(
    () =>
      [
        ["overview", "Overview"],
        ["launch", "Launch workflow"],
        ["jobs", "Jobs"],
        ["character", "Character gates"],
        ["settings", "Settings"]
      ] as const,
    []
  );

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            G
          </span>
          <div>
            <strong>GenAImage</strong>
            <small>ComfyUI harness</small>
          </div>
        </div>
        <nav aria-label="Application sections">
          {selectedNav.map(([id, label]) => (
            <button
              className={section === id ? "nav-button active" : "nav-button"}
              key={id}
              type="button"
              onClick={() => setSection(id)}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="sidebar-status">
          <span
            className={`status-dot ${healthy ? "online" : healthy === false ? "offline" : ""}`}
            aria-hidden="true"
          />
          <span>{healthy ? "ComfyUI connected" : healthy === false ? "ComfyUI unavailable" : "Checking"}</span>
          <button className="text-button" type="button" onClick={() => void checkHealth()}>
            Retry
          </button>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div>
            <p className="eyebrow">Local workspace</p>
            <h1>{section === "overview" ? "Production overview" : selectedNav.find(([id]) => id === section)?.[1]}</h1>
          </div>
          <button
            className="secondary-button"
            type="button"
            onClick={() => void refresh()}
            disabled={refreshing}
          >
            {refreshing ? "Refreshing…" : "Refresh"}
          </button>
        </header>

        {error && (
          <div className="error-banner" role="alert">
            <span>{error}</span>
            <button type="button" aria-label="Dismiss error" onClick={() => setError("")}>
              ×
            </button>
          </div>
        )}

        {section === "overview" && (
          <Overview
            config={config}
            healthy={healthy}
            runs={runs}
            workflows={workflows}
            onNavigate={setSection}
          />
        )}
        {section === "launch" && (
          <LaunchPanel
            workflows={workflows}
            onError={setError}
            onSubmitted={(run) => {
              setRuns((current) => [run, ...current.filter((item) => item.id !== run.id)]);
              setSection("jobs");
            }}
          />
        )}
        {section === "jobs" && (
          <JobsPanel
            runs={runs}
            onRefresh={refresh}
            onError={setError}
            onUpdate={(run) =>
              setRuns((current) => [run, ...current.filter((item) => item.id !== run.id)])
            }
          />
        )}
        {section === "character" && <CharacterPanel onError={setError} />}
        {section === "settings" && (
          <SettingsPanel
            config={config}
            workspace={workspace}
            onWorkspaceChanged={(nextWorkspace) => {
              setWorkspace(nextWorkspace);
              void refresh();
              void checkHealth();
            }}
            onConfigChanged={setConfig}
            onError={setError}
          />
        )}
      </main>
    </div>
  );
}

function Overview({
  config,
  healthy,
  runs,
  workflows,
  onNavigate
}: {
  config: ResolvedConfig | null;
  healthy: boolean | null;
  runs: RunRecord[];
  workflows: WorkflowManifest[];
  onNavigate: (section: Section) => void;
}) {
  const active = runs.filter((run) => isTrackable(run.status)).length;
  const completed = runs.filter((run) => run.status === "completed").length;
  return (
    <div className="stack">
      <section className="hero-card">
        <div>
          <p className="eyebrow">Controlled local generation</p>
          <h2>Run approved workflows, inspect every setting.</h2>
          <p className="lede">
            GenAImage submits API-format workflows to an external local ComfyUI host and
            records queue state, gate decisions, source hashes, and outputs.
          </p>
          <div className="button-row">
            <button className="primary-button" type="button" onClick={() => onNavigate("launch")}>
              Launch workflow
            </button>
            <button className="secondary-button" type="button" onClick={() => onNavigate("character")}>
              Review gates
            </button>
          </div>
        </div>
        <div className="connection-panel">
          <span className="connection-label">ComfyUI</span>
          <strong>{healthy ? "Ready" : healthy === false ? "Unavailable" : "Checking"}</strong>
          <code>{config?.comfyui.base_url ?? "No profile loaded"}</code>
        </div>
      </section>
      <section className="metric-grid" aria-label="Harness summary">
        <Metric label="Workflows" value={String(workflows.length)} />
        <Metric label="Active jobs" value={String(active)} />
        <Metric label="Completed jobs" value={String(completed)} />
        <Metric label="Failed or unknown" value={String(runs.filter((run) => ["failed", "unknown_submission"].includes(run.status)).length)} />
      </section>
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">Recent work</p>
            <h2>Latest runs</h2>
          </div>
          <button className="text-button" type="button" onClick={() => onNavigate("jobs")}>
            View all jobs
          </button>
        </div>
        {runs.length === 0 ? (
          <EmptyState title="No runs yet" copy="Validate a workflow and launch your first explicit run." />
        ) : (
          <div className="compact-list">
            {runs.slice(0, 5).map((run) => (
              <div className="compact-row" key={run.id}>
                <div>
                  <strong>{run.workflow_id}</strong>
                  <span>{run.id}</span>
                </div>
                <StatusPill status={run.status} />
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function LaunchPanel({
  workflows,
  onSubmitted,
  onError
}: {
  workflows: WorkflowManifest[];
  onSubmitted: (run: RunRecord) => void;
  onError: (message: string) => void;
}) {
  const [selected, setSelected] = useState("");
  const [inspection, setInspection] = useState<WorkflowInspection | null>(null);
  const [preset, setPreset] = useState("");
  const [values, setValues] = useState<Record<string, string>>({});
  const [characterId, setCharacterId] = useState("");
  const [version, setVersion] = useState("");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (workflows.length > 0 && !selected) {
      setSelected(workflows[0].id);
    }
  }, [selected, workflows]);

  useEffect(() => {
    if (!selected) {
      setInspection(null);
      return;
    }
    void api
      .inspectWorkflow(selected)
      .then((result) => {
        setInspection(result);
        setPreset("");
        setValues(
          Object.fromEntries(
            Object.entries(result.manifest.parameters).map(([name, definition]) => [
              name,
              formValue(definition.default)
            ])
          )
        );
      })
      .catch((caught) => onError(errorMessage(caught)));
  }, [onError, selected]);

  function applyPreset(nextPreset: string) {
    setPreset(nextPreset);
    if (!inspection || !nextPreset) {
      return;
    }
    const presetValues = inspection.manifest.presets[nextPreset] ?? {};
    setValues((current) => ({
      ...current,
      ...Object.fromEntries(
        Object.entries(presetValues).map(([name, value]) => [name, formValue(value)])
      )
    }));
  }

  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!inspection) {
      return;
    }
    try {
      const parameters: Record<string, JsonValue> = {};
      for (const [name, definition] of Object.entries(inspection.manifest.parameters)) {
        const raw = values[name] ?? "";
        if (raw === "" && !definition.required) {
          continue;
        }
        if (raw === "" && definition.required) {
          throw new Error(`Required parameter "${name}" is empty.`);
        }
        parameters[name] = parseParameterValue(definition, raw);
      }
      setSubmitting(true);
      const run = await api.submitRun({
        workflow: selected,
        preset: preset || null,
        parameters,
        character_id: characterId.trim() || null,
        version: version.trim() || null
      });
      onSubmitted(run);
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form className="stack" onSubmit={(event) => void submit(event)}>
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">Workflow contract</p>
            <h2>Select a manifest</h2>
          </div>
          <span className="mono">{inspection ? shortHash(inspection.workflow_sha256) : "—"}</span>
        </div>
        <label className="field">
          <span>Workflow</span>
          <select value={selected} onChange={(event) => setSelected(event.target.value)}>
            {workflows.map((workflow) => (
              <option key={workflow.id} value={workflow.id}>
                {workflow.title || workflow.id} · v{workflow.version}
              </option>
            ))}
          </select>
        </label>
        {inspection && (
          <div className="workflow-meta">
            <span>{inspection.manifest.generation_mode.replaceAll("_", " ")}</span>
            <span>{inspection.manifest.workflow}</span>
            <span>
              {inspection.required_gates.length > 0
                ? `Required: ${inspection.required_gates.join(", ")}`
                : "No character gate required"}
            </span>
          </div>
        )}
      </section>

      {inspection && (
        <>
          <section className="card">
            <div className="card-heading">
              <div>
                <p className="eyebrow">Explicit inputs</p>
                <h2>Parameters</h2>
              </div>
              {Object.keys(inspection.manifest.presets).length > 0 && (
                <label className="compact-field">
                  <span>Preset</span>
                  <select value={preset} onChange={(event) => applyPreset(event.target.value)}>
                    <option value="">None</option>
                    {Object.keys(inspection.manifest.presets).map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </div>
            <div className="form-grid">
              {Object.entries(inspection.manifest.parameters).map(([name, definition]) => (
                <label className="field" key={name}>
                  <span>
                    {name}
                    {definition.required && <b aria-label="required"> *</b>}
                  </span>
                  {definition.kind === "boolean" ? (
                    <select
                      value={values[name] ?? "false"}
                      onChange={(event) => setValues((current) => ({ ...current, [name]: event.target.value }))}
                    >
                      <option value="false">False</option>
                      <option value="true">True</option>
                    </select>
                  ) : name.toLowerCase().includes("prompt") ? (
                    <textarea
                      value={values[name] ?? ""}
                      rows={4}
                      onChange={(event) => setValues((current) => ({ ...current, [name]: event.target.value }))}
                    />
                  ) : definition.allowed_values ? (
                    <select
                      value={values[name] ?? ""}
                      onChange={(event) => setValues((current) => ({ ...current, [name]: event.target.value }))}
                    >
                      {definition.allowed_values.map((option) => (
                        <option key={formValue(option)} value={formValue(option)}>
                          {formValue(option)}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <input
                      type={["integer", "number", "seed"].includes(definition.kind) ? "number" : "text"}
                      min={definition.minimum ?? undefined}
                      max={definition.maximum ?? undefined}
                      step={definition.kind === "number" ? "any" : undefined}
                      placeholder={definition.kind === "image_upload" ? "/absolute/path/to/reference.png" : ""}
                      value={values[name] ?? ""}
                      onChange={(event) => setValues((current) => ({ ...current, [name]: event.target.value }))}
                    />
                  )}
                  <small>{definition.description || `${definition.kind} → node ${definition.node}.${definition.input}`}</small>
                </label>
              ))}
            </div>
          </section>

          <section className="card">
            <div className="card-heading">
              <div>
                <p className="eyebrow">Run identity</p>
                <h2>Context and submission</h2>
              </div>
            </div>
            <div className="form-grid two-columns">
              <label className="field">
                <span>Character ID</span>
                <input
                  value={characterId}
                  onChange={(event) => setCharacterId(event.target.value)}
                  placeholder={inspection.required_gates.length ? "Required, e.g. CHR-001" : "Optional"}
                />
              </label>
              <label className="field">
                <span>Production version</span>
                <input
                  value={version}
                  onChange={(event) => setVersion(event.target.value)}
                  placeholder="Uses run ID if empty"
                />
              </label>
            </div>
            {inspection.required_gates.length > 0 && (
              <p className="notice">
                This workflow is blocked unless the character has the required explicit approvals:
                {" "}{inspection.required_gates.join(", ")}.
              </p>
            )}
            <div className="button-row">
              <button className="primary-button" type="submit" disabled={submitting || workflows.length === 0}>
                {submitting ? "Submitting…" : "Validate and submit"}
              </button>
            </div>
          </section>
        </>
      )}
    </form>
  );
}

function JobsPanel({
  runs,
  onUpdate,
  onRefresh,
  onError
}: {
  runs: RunRecord[];
  onUpdate: (run: RunRecord) => void;
  onRefresh: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const [details, setDetails] = useState<RunDetails | null>(null);
  const [busy, setBusy] = useState("");

  async function openRun(runId: string) {
    setSelected(runId);
    setBusy(runId);
    try {
      setDetails(await api.runDetails(runId));
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy("");
    }
  }

  async function reconcile(runId: string | null = null) {
    setBusy(runId ?? "all");
    try {
      await api.reconcileRuns(runId);
      await onRefresh();
      if (runId && selected === runId) {
        setDetails(await api.runDetails(runId));
      }
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy("");
    }
  }

  async function cancel(run: RunRecord) {
    if (!window.confirm(`Cancel run ${run.id}? Running work on the local ComfyUI host will be interrupted.`)) {
      return;
    }
    setBusy(run.id);
    try {
      onUpdate(await api.cancelRun(run.id));
      await reconcile(run.id);
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy("");
    }
  }

  return (
    <div className="stack">
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">SQLite-backed queue</p>
            <h2>Local run history</h2>
          </div>
          <button
            className="secondary-button"
            type="button"
            disabled={busy === "all"}
            onClick={() => void reconcile()}
          >
            {busy === "all" ? "Reconciling…" : "Reconcile all"}
          </button>
        </div>
        {runs.length === 0 ? (
          <EmptyState title="No jobs recorded" copy="Submitted runs and reconnect state will appear here." />
        ) : (
          <div className="jobs-table" role="table" aria-label="ComfyUI jobs">
            <div className="jobs-header" role="row">
              <span>Workflow / run</span>
              <span>Context</span>
              <span>Status</span>
              <span>Updated</span>
              <span>Actions</span>
            </div>
            {runs.map((run) => (
              <div className={selected === run.id ? "job-row selected" : "job-row"} role="row" key={run.id}>
                <button className="run-link" type="button" onClick={() => void openRun(run.id)}>
                  <strong>{run.workflow_id}</strong>
                  <span>{run.id}</span>
                </button>
                <span>{run.character_id ?? run.generation_mode.replaceAll("_", " ")}</span>
                <StatusPill status={run.status} />
                <span>{new Date(run.updated_at).toLocaleString("en-GB")}</span>
                <div className="row-actions">
                  {isTrackable(run.status) && (
                    <button className="danger-button" type="button" disabled={busy === run.id} onClick={() => void cancel(run)}>
                      Cancel
                    </button>
                  )}
                  <button className="text-button" type="button" disabled={busy === run.id} onClick={() => void reconcile(run.id)}>
                    Reconcile
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {details && selected === details.run.id && (
        <RunDetail details={details} busy={busy === selected} />
      )}
    </div>
  );
}

function RunDetail({ details, busy }: { details: RunDetails; busy: boolean }) {
  return (
    <section className="card" aria-busy={busy}>
      <div className="card-heading">
        <div>
          <p className="eyebrow">Run detail</p>
          <h2>{details.run.workflow_id}</h2>
        </div>
        <StatusPill status={details.run.status} />
      </div>
      <div className="detail-grid">
        <Detail label="Run ID" value={details.run.id} />
        <Detail label="Prompt ID" value={details.run.prompt_id} />
        <Detail label="Workflow" value={`${details.run.workflow_version} · ${shortHash(details.run.workflow_sha256)}`} />
        <Detail label="Output directory" value={details.run.output_dir} />
      </div>
      {details.run.error && <p className="notice error">{details.run.error}</p>}
      <div className="subsection">
        <h3>Captured outputs</h3>
        {details.outputs.length === 0 ? (
          <p className="muted">No captured outputs yet.</p>
        ) : (
          <div className="output-grid">
            {details.outputs.map((output) => (
              <article className="output-card" key={`${output.node_id}-${output.source_filename}`}>
                <span className="output-kind">{output.kind}</span>
                <strong>{output.source_filename}</strong>
                <code>{output.local_path}</code>
                <small>{formatBytes(output.byte_size)} · SHA-256 {shortHash(output.sha256)}</small>
              </article>
            ))}
          </div>
        )}
      </div>
      <div className="subsection">
        <h3>Event history</h3>
        <div className="timeline">
          {details.events.map((event) => (
            <div className="timeline-item" key={event.id}>
              <span className="timeline-dot" />
              <div>
                <strong>{event.event_type.replaceAll("_", " ")}</strong>
                <p>{event.message ?? "Event recorded"}</p>
                <small>{new Date(event.created_at).toLocaleString("en-GB")}</small>
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

function CharacterPanel({ onError }: { onError: (message: string) => void }) {
  const [characterId, setCharacterId] = useState("CHR-001");
  const [name, setName] = useState("");
  const [state, setState] = useState<CharacterState | null>(null);
  const [approver, setApprover] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  async function load() {
    setBusy(true);
    try {
      setState(await api.characterStatus(characterId.trim()));
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }

  async function initialise() {
    setBusy(true);
    try {
      setState(await api.initialiseCharacter(characterId.trim(), name.trim() || null));
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }

  async function decide(gate: GateId, approved: boolean) {
    if (!approver.trim() || !note.trim()) {
      onError("Approver and decision note are required for every gate decision.");
      return;
    }
    if (!window.confirm(`${approved ? "Approve" : "Reject"} ${gateLabels[gate]} for ${characterId}?`)) {
      return;
    }
    setBusy(true);
    try {
      setState(await api.decideGate(characterId.trim(), gate, approver.trim(), note.trim(), approved));
      setNote("");
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="stack">
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">Portable approval state</p>
            <h2>Character package</h2>
          </div>
        </div>
        <div className="form-grid three-columns">
          <label className="field">
            <span>Character ID</span>
            <input value={characterId} onChange={(event) => setCharacterId(event.target.value)} />
          </label>
          <label className="field">
            <span>Name for a new package</span>
            <input value={name} onChange={(event) => setName(event.target.value)} />
          </label>
          <div className="button-row align-end">
            <button className="secondary-button" type="button" disabled={busy || !characterId.trim()} onClick={() => void load()}>
              Load
            </button>
            <button className="primary-button" type="button" disabled={busy || !characterId.trim()} onClick={() => void initialise()}>
              Initialise
            </button>
          </div>
        </div>
      </section>

      {state && (
        <>
          <section className="card">
            <div className="card-heading">
              <div>
                <p className="eyebrow">{state.character_id}</p>
                <h2>{state.character_name || "Unnamed character"}</h2>
              </div>
              <span className="mono">Current {state.current_version}</span>
            </div>
            <div className="form-grid two-columns">
              <label className="field">
                <span>Approver</span>
                <input value={approver} onChange={(event) => setApprover(event.target.value)} placeholder="Explicit human name" />
              </label>
              <label className="field">
                <span>Decision note / conditions</span>
                <input value={note} onChange={(event) => setNote(event.target.value)} placeholder="Why this gate was approved or rejected" />
              </label>
            </div>
            <div className="gate-list">
              {gateOrder.map((gate) => {
                const record = state.gates[gate];
                return (
                  <div className="gate-row" key={gate}>
                    <div>
                      <strong>{gateLabels[gate]}</strong>
                      <small>{record?.decision ?? "No decision recorded"}</small>
                    </div>
                    <span className={`gate-status ${record?.status ?? "pending"}`}>
                      {record?.status ?? "pending"}
                    </span>
                    <div className="row-actions">
                      <button className="primary-button compact" type="button" disabled={busy} onClick={() => void decide(gate, true)}>
                        Approve
                      </button>
                      <button className="danger-button compact" type="button" disabled={busy} onClick={() => void decide(gate, false)}>
                        Reject
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
          <section className="card">
            <div className="card-heading">
              <div>
                <p className="eyebrow">Immutable audit trail</p>
                <h2>Decision history</h2>
              </div>
            </div>
            {state.decisions.length === 0 ? (
              <EmptyState title="No decisions" copy="Every explicit approval or rejection will be listed here." />
            ) : (
              <div className="timeline">
                {[...state.decisions].reverse().map((decision) => (
                  <div className="timeline-item" key={`${decision.gate}-${decision.decided_at}`}>
                    <span className="timeline-dot" />
                    <div>
                      <strong>{gateLabels[decision.gate]} · {decision.status}</strong>
                      <p>{decision.decision}</p>
                      <small>{decision.approver} · {new Date(decision.decided_at).toLocaleString("en-GB")}</small>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>
        </>
      )}
    </div>
  );
}

function SettingsPanel({
  config,
  workspace,
  onWorkspaceChanged,
  onConfigChanged,
  onError
}: {
  config: ResolvedConfig | null;
  workspace: string;
  onWorkspaceChanged: (workspace: string) => void;
  onConfigChanged: (config: ResolvedConfig) => void;
  onError: (message: string) => void;
}) {
  const [workspaceValue, setWorkspaceValue] = useState(workspace);
  const [baseUrl, setBaseUrl] = useState(config?.comfyui.base_url ?? "");
  const [busy, setBusy] = useState(false);

  useEffect(() => setWorkspaceValue(workspace), [workspace]);
  useEffect(() => setBaseUrl(config?.comfyui.base_url ?? ""), [config]);

  async function saveWorkspace() {
    setBusy(true);
    try {
      onWorkspaceChanged(await api.setWorkspace(workspaceValue));
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }

  async function saveUrl() {
    if (!config) {
      return;
    }
    setBusy(true);
    try {
      onConfigChanged(await api.setComfyUiUrl(config.active_profile, baseUrl));
    } catch (caught) {
      onError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="stack">
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">Local project</p>
            <h2>Workspace</h2>
          </div>
        </div>
        <label className="field">
          <span>Repository or project root</span>
          <input value={workspaceValue} onChange={(event) => setWorkspaceValue(event.target.value)} />
          <small>The desktop must be pointed at a GenAImage workspace containing workflows and templates.</small>
        </label>
        <button className="primary-button" type="button" disabled={busy} onClick={() => void saveWorkspace()}>
          Use workspace
        </button>
      </section>
      <section className="card">
        <div className="card-heading">
          <div>
            <p className="eyebrow">ComfyUI profile</p>
            <h2>Local server</h2>
          </div>
        </div>
        <div className="form-grid two-columns">
          <label className="field">
            <span>Profile</span>
            <input value={config?.active_profile ?? ""} disabled />
          </label>
          <label className="field">
            <span>Base URL</span>
            <input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} />
            <small>Only loopback hosts are accepted in this release.</small>
          </label>
        </div>
        <button className="primary-button" type="button" disabled={busy || !config} onClick={() => void saveUrl()}>
          Save local profile
        </button>
      </section>
      {config && (
        <section className="card">
          <div className="card-heading">
            <div>
              <p className="eyebrow">Resolved paths</p>
              <h2>Storage</h2>
            </div>
          </div>
          <div className="detail-grid">
            <Detail label="State database" value={config.state_database} />
            <Detail label="Workflows" value={config.workflows} />
            <Detail label="Runs" value={config.runs} />
            <Detail label="Characters" value={config.characters} />
          </div>
        </section>
      )}
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric-card">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function StatusPill({ status }: { status: RunRecord["status"] }) {
  return <span className={`status-pill ${status}`}>{statusLabel(status)}</span>;
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div className="detail">
      <span>{label}</span>
      <code>{value}</code>
    </div>
  );
}

function EmptyState({ title, copy }: { title: string; copy: string }) {
  return (
    <div className="empty-state">
      <strong>{title}</strong>
      <p>{copy}</p>
    </div>
  );
}
