<script lang="ts">
  import { untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { api, ApiProblemError } from '../../lib/api/client';
  import { events, type EventEnvelope } from '../../lib/api/events';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import Dialog from '../../lib/ui/Dialog.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import { navigateHash } from '../../navigation';
  import StatusBadge from '../jobs/StatusBadge.svelte';
  import type { Run } from '../jobs/run-model';
  import { formatRecordedValue } from '../gallery/parameter-diff';
  import Thumbnail from '../gallery/Thumbnail.svelte';
  import { buildGenerateUrl } from './remix';

  let { id }: { id: string } = $props();
  type Output = components['schemas']['OutputResponse'];
  type RunEvent = components['schemas']['RunEventResponse'];
  let run = $state.raw<Run>();
  let timeline = $state.raw<RunEvent[]>([]);
  let outputs = $state.raw<Output[]>([]);
  let relatives = $state.raw<Run[]>([]);
  let moreRelatives = $state(false);
  let manifest = $state.raw<Record<string, unknown>>();
  let manifestText = $state<string>();
  let manifestVerified = $state(false);
  let manifestError = $state<Problem>();
  let error = $state<Problem>();
  let auxiliaryError = $state<Problem>();
  let loading = $state(true);
  let open = $state(false);
  let showResolved = $state(false);
  let outputCursor = $state<string>();
  let loadingOutputs = $state(false);
  let storageAvailable = $state(true);
  let busy = $state(false);
  let actionError = $state<Problem>();
  let mismatch = $state(false);
  let pendingKey = $state<string>();
  let actionNotice = $state<string>();
  let heading: HTMLHeadingElement;
  let generation = 0;
  let inFlight = false;
  let dirty = false;
  let lifecycle = 0;
  let controller: AbortController;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let rerunUrl = $derived(run ? buildGenerateUrl(run) : undefined);
  let parameters = $derived(
    manifest && Object.hasOwn(manifest, 'parameters')
      ? manifest.parameters
      : run?.parameters,
  );
  let workflow = $derived(asObject(manifest?.workflow));
  let workflowId = $derived(
    typeof workflow?.id === 'string' ? workflow.id : run?.workflow_id,
  );
  let workflowVersion = $derived(
    typeof workflow?.version === 'string'
      ? workflow.version
      : run?.workflow_version,
  );
  let workflowHash = $derived(
    typeof workflow?.sha256 === 'string'
      ? workflow.sha256
      : run?.workflow_sha256,
  );
  let inputs = $derived(
    Array.isArray(manifest?.source_inputs) ? manifest.source_inputs : [],
  );
  let manifestReady = $derived(
    !!manifest &&
      !!run &&
      manifestVerified &&
      Object.hasOwn(manifest, 'parameters') &&
      sameRecorded(manifest.parameters, run.parameters) &&
      workflow?.id === run.workflow_id &&
      workflow?.version === run.workflow_version &&
      workflow?.sha256 === run.workflow_sha256 &&
      Object.hasOwn(manifest, 'preset') &&
      (manifest.preset === null || typeof manifest.preset === 'string') &&
      !hasInexactNumber(parameters) &&
      Array.isArray(manifest.source_inputs) &&
      inputs.every(
        (input) =>
          typeof asObject(input)?.parameter === 'string' &&
          typeof asObject(input)?.sha256 === 'string',
      ),
  );

  let resolved = $state.raw<unknown>();
  let resolvedError = $state<Problem>();
  let resolvedLoading = $state(false);
  let connection = events.status;

  function asObject(value: unknown): Record<string, unknown> | undefined {
    return value !== null && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  }
  function hasInexactNumber(value: unknown): boolean {
    if (typeof value === 'number')
      return (
        !Number.isFinite(value) ||
        (Number.isInteger(value) && !Number.isSafeInteger(value))
      );
    if (Array.isArray(value)) return value.some(hasInexactNumber);
    return Object.values(asObject(value) ?? {}).some(hasInexactNumber);
  }
  function sameRecorded(left: unknown, right: unknown): boolean {
    if (Object.is(left, right)) return true;
    if (Array.isArray(left) && Array.isArray(right))
      return (
        left.length === right.length &&
        left.every((item, index) => sameRecorded(item, right[index]))
      );
    const a = asObject(left);
    const b = asObject(right);
    return (
      !!a &&
      !!b &&
      Object.keys(a).length === Object.keys(b).length &&
      Object.keys(a).every(
        (key) => Object.hasOwn(b, key) && sameRecorded(a[key], b[key]),
      )
    );
  }
  function issue(value: unknown, title: string): Problem {
    return value instanceof ApiProblemError
      ? value.problem
      : {
          title,
          detail: value instanceof Error ? value.message : undefined,
          recovery: 'Check the local server and try again.',
        };
  }
  function exactJson(text: string): Record<string, unknown> | undefined {
    return asObject(
      JSON.parse(
        text,
        (_key, value: unknown, context?: { source?: string }) => {
          if (
            typeof value === 'number' &&
            Number.isInteger(value) &&
            !Number.isSafeInteger(value) &&
            context?.source &&
            /^-?\d+$/.test(context.source)
          )
            return BigInt(context.source);
          return value;
        },
      ),
    );
  }
  function path(runId: string) {
    return `#/runs/${encodeURIComponent(runId)}`;
  }
  function gallery(id: string) {
    return `#/gallery?${new URLSearchParams({ output: id })}`;
  }
  function storageKey(runId: string) {
    return `genaimage.rerun.${runId}`;
  }
  function definitive(error: unknown) {
    return (
      error instanceof ApiProblemError &&
      ((error.type === 'urn:genaimage:problem:rerun-mismatch' &&
        error.status === 409) ||
        (error.type === 'about:blank' &&
          [400, 403, 404, 409, 413, 421, 422].includes(error.status)))
    );
  }

  async function load(runId: string, signal: AbortSignal) {
    const current = ++generation;
    loading = !run;
    error = auxiliaryError = undefined;
    try {
      const record = await api.getRecordedRun(runId, { signal });
      if (signal.aborted || current !== generation) return;
      run = record;
      loading = false;
      const requests = await Promise.allSettled([
        api.listRunEvents(runId, { limit: 200 }, { signal }),
        api.listOutputs(
          { run: runId, status: record.status, limit: 200 },
          { signal },
        ),
        api.listRuns({ limit: 200 }, { signal }),
        api.getRunManifestText(runId, { signal }),
      ]);
      if (signal.aborted || current !== generation) return;
      const [eventResult, outputResult, runResult, manifestResult] = requests;
      if (eventResult.status === 'fulfilled') {
        const merged = new SvelteMap(timeline.map((item) => [item.id, item]));
        for (const item of eventResult.value.events) merged.set(item.id, item);
        timeline = [...merged.values()].sort((a, b) => a.id - b.id).slice(-500);
      }
      if (outputResult.status === 'fulfilled') {
        outputs = outputResult.value.outputs;
        outputCursor = outputResult.value.next_cursor ?? undefined;
      }
      if (runResult.status === 'fulfilled') {
        relatives = runResult.value.runs.filter(
          (item) => item.rerun_of === runId,
        );
        moreRelatives = !!runResult.value.next_cursor;
      }
      if (requests.slice(0, 3).some((item) => item.status === 'rejected'))
        auxiliaryError = {
          title: 'Some run details could not be loaded',
          recovery: 'Reload this run to try again.',
        };
      if (manifestResult.status === 'fulfilled') {
        try {
          const parsed = exactJson(manifestResult.value.text);
          if (!parsed) throw new Error('The manifest is not a JSON object.');
          manifestText = manifestResult.value.text;
          manifestVerified = manifestResult.value.integrityVerified;
          manifest = parsed;
          manifestError = undefined;
        } catch (reason) {
          manifestText = undefined;
          manifestVerified = false;
          manifest = undefined;
          manifestError = issue(reason, 'Manifest unavailable');
        }
      } else {
        manifestText = undefined;
        manifestVerified = false;
        manifest = undefined;
        manifestError = issue(manifestResult.reason, 'Manifest unavailable');
      }
    } catch (reason) {
      if (signal.aborted || current !== generation) return;
      error = issue(reason, 'Could not load run');
      loading = false;
    }
  }
  function refresh() {
    clearTimeout(timer);
    timer = undefined;
    controller?.abort();
    loadingOutputs = false;
    const request = new AbortController();
    controller = request;
    inFlight = true;
    dirty = false;
    void load(id, request.signal).finally(() => {
      if (controller !== request || request.signal.aborted) return;
      inFlight = false;
      if (dirty) {
        dirty = false;
        schedule();
      }
    });
  }
  async function moreOutputPage() {
    if (!run || !outputCursor || loadingOutputs) return;
    const cursor = outputCursor;
    const current = generation;
    loadingOutputs = true;
    try {
      const page = await api.listOutputs(
        { run: run.id, status: run.status, limit: 200, cursor },
        { signal: controller.signal },
      );
      if (current !== generation) return;
      const seen = new Set(outputs.map((item) => item.id));
      outputs = [
        ...outputs,
        ...page.outputs.filter((item) => !seen.has(item.id)),
      ];
      outputCursor = page.next_cursor ?? undefined;
    } catch (reason) {
      if (current === generation)
        auxiliaryError = issue(reason, 'Could not load more outputs');
    } finally {
      if (current === generation) loadingOutputs = false;
    }
  }
  function schedule() {
    if (inFlight) {
      dirty = true;
      return;
    }
    timer ??= setTimeout(refresh, 100);
  }
  $effect(() => {
    const runId = id;
    run = undefined;
    timeline = [];
    outputs = [];
    outputCursor = undefined;
    loadingOutputs = false;
    relatives = [];
    manifest = undefined;
    manifestText = undefined;
    manifestVerified = false;
    actionError = undefined;
    mismatch = false;
    actionNotice = undefined;
    busy = false;
    open = false;
    showResolved = false;
    resolved = undefined;
    resolvedError = undefined;
    try {
      pendingKey = sessionStorage.getItem(storageKey(runId)) ?? undefined;
      storageAvailable = true;
    } catch {
      pendingKey = undefined;
      storageAvailable = false;
    }
    let opened = false;
    const types = [
      'run_queued',
      'run_started',
      'run_progress',
      'run_output',
      'run_completed',
      'run_failed',
      'run_cancelled',
      'run_unknown_submission',
    ] as const;
    const cleanup = types.map((type) =>
      events.on(type, (event: EventEnvelope) => {
        if (
          ('run' in event && event.run.id === runId) ||
          ('run_id' in event && event.run_id === runId)
        )
          schedule();
        else if ('run' in event && event.run.id !== runId) schedule();
      }),
    );
    cleanup.push(
      events.status.subscribe((status) => {
        if (status === 'open' && !opened) {
          opened = true;
          schedule();
        }
      }),
    );
    cleanup.push(
      events.resync.subscribe((signal) => {
        if (signal) schedule();
      }),
    );
    untrack(refresh);
    return () => {
      generation++;
      lifecycle++;
      inFlight = false;
      dirty = false;
      controller?.abort();
      clearTimeout(timer);
      cleanup.forEach((dispose) => dispose());
    };
  });
  $effect(() => {
    const runId = id;
    if (!showResolved) return;
    const history = asObject(manifest?.comfyui_history);
    const promptId =
      typeof manifest?.prompt_id === 'string'
        ? manifest.prompt_id
        : run?.prompt_id;
    const prompt = promptId ? asObject(history?.[promptId])?.prompt : undefined;
    const recorded = Array.isArray(prompt) ? asObject(prompt[2]) : undefined;
    if (recorded) {
      resolved = recorded;
      resolvedError = undefined;
      resolvedLoading = false;
      return;
    }
    const request = new AbortController();
    resolvedLoading = true;
    resolvedError = undefined;
    api
      .getResolvedWorkflow(runId, { signal: request.signal })
      .then((graph) => {
        if (!request.signal.aborted) resolved = graph;
      })
      .catch((reason: unknown) => {
        if (!request.signal.aborted)
          resolvedError = issue(reason, 'Could not load the resolved workflow');
      })
      .finally(() => {
        if (!request.signal.aborted) resolvedLoading = false;
      });
    return () => request.abort();
  });
  function download() {
    if (!manifestText) return;
    const url = URL.createObjectURL(
      new Blob([manifestText], { type: 'application/json' }),
    );
    const link = document.createElement('a');
    link.href = url;
    link.download = `run-${id.replace(/[^a-zA-Z0-9._-]/g, '_')}.manifest.json`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
  async function submit(retry = false) {
    if (!run || busy) return;
    open = false;
    busy = true;
    actionError = undefined;
    mismatch = false;
    actionNotice = undefined;
    const runId = run.id;
    const actionLifecycle = lifecycle;
    let sent = false;
    try {
      const latest = await api.getRun(runId);
      if (actionLifecycle !== lifecycle) return;
      if (latest.status === 'unknown_submission') {
        actionNotice =
          'The source submission is uncertain. Reconcile it in Jobs before submitting as a new run.';
        refresh();
        return;
      }
      let key = retry ? pendingKey : undefined;
      if (!key) {
        key = crypto.randomUUID();
        sessionStorage.setItem(storageKey(runId), key);
        pendingKey = key;
      }
      sent = true;
      const result = await api.rerunRun(runId, key);
      try {
        sessionStorage.removeItem(storageKey(runId));
      } catch {
        /* The same key remains safe to replay. */
      }
      if (actionLifecycle !== lifecycle) return;
      pendingKey = undefined;
      navigateHash(`${path(result.id)}?created=rerun`);
    } catch (reason) {
      if (actionLifecycle !== lifecycle) return;
      if (sent && definitive(reason)) {
        try {
          sessionStorage.removeItem(storageKey(runId));
          pendingKey = undefined;
        } catch {
          storageAvailable = false;
        }
        mismatch =
          reason instanceof ApiProblemError &&
          reason.type === 'urn:genaimage:problem:rerun-mismatch';
        actionNotice = 'No new run was created.';
      } else {
        actionNotice = pendingKey
          ? 'The result is uncertain. Do not start a new attempt; check Jobs or retry with the same key.'
          : 'The request was not submitted. Check your browser storage and try again.';
      }
      actionError = issue(reason, 'Could not confirm the new run');
    } finally {
      if (actionLifecycle === lifecycle) busy = false;
    }
  }
</script>

<h1 tabindex="-1" bind:this={heading}>Run detail</h1>
{#if loading}<p role="status">Loading run…</p>{/if}
{#if error}<ProblemDetails problem={error} /><Button onclick={refresh}
    >Reload run</Button
  >{/if}
{#if run}
  {#if new URLSearchParams(location.hash.split('?')[1] ?? '').get('created') === 'rerun'}<p
      role="status"
    >
      New run recorded. The original run is unchanged; generation is not yet
      confirmed.
    </p>{/if}
  <p>
    <strong>{run.id}</strong>
    {#if run.status === 'completed' && !outputs.length}
      <span>Completed · outputs not confirmed</span>
    {:else}<StatusBadge status={run.status} />{/if}
  </p>
  <p class="muted">
    {$connection === 'open'
      ? 'Live updates connected.'
      : 'Live updates are not connected.'}
  </p>
  {#if auxiliaryError}<ProblemDetails problem={auxiliaryError} /><Button
      onclick={refresh}>Reload details</Button
    >{/if}
  <h2>Provenance</h2>
  {#if manifest && !manifestVerified}<p>
      Manifest integrity could not be verified.
    </p>{/if}
  <dl>
    <div>
      <dt>Workflow</dt>
      <dd>{workflowId}</dd>
    </div>
    <div>
      <dt>Workflow version</dt>
      <dd>{workflowVersion}</dd>
    </div>
    <div>
      <dt>Workflow SHA-256</dt>
      <dd>{workflowHash}</dd>
    </div>
    {#if workflow?.manifest_sha256}<div>
        <dt>Workflow manifest SHA-256</dt>
        <dd>{formatRecordedValue(workflow.manifest_sha256)}</dd>
      </div>{/if}
    {#if manifest && 'preset' in manifest}<div>
        <dt>Preset</dt>
        <dd>
          {manifest.preset === null
            ? 'None'
            : formatRecordedValue(manifest.preset)}
        </dd>
      </div>{/if}
    <div>
      <dt>ComfyUI prompt ID</dt>
      <dd>{run.prompt_id}</dd>
    </div>
    {#if manifest?.comfyui_base_url}<div>
        <dt>ComfyUI endpoint</dt>
        <dd>{formatRecordedValue(manifest.comfyui_base_url)}</dd>
      </div>{/if}
    {#if manifest?.client_id}<div>
        <dt>Client ID</dt>
        <dd>{formatRecordedValue(manifest.client_id)}</dd>
      </div>{/if}
    {#if run.rerun_of}<div>
        <dt>Re-run of</dt>
        <dd><a href={path(run.rerun_of)}>{run.rerun_of}</a></dd>
      </div>{/if}
  </dl>
  <h3>Recorded parameters</h3>
  {#if asObject(parameters)}
    <dl>
      {#each Object.entries(asObject(parameters) ?? {}) as [name, value] (name)}<div
        >
          <dt>{name === 'seed' ? 'Seed' : name}</dt>
          <dd>{formatRecordedValue(value)}</dd>
        </div>{/each}
    </dl>
  {:else}<pre>{formatRecordedValue(parameters)}</pre>{/if}
  <h3>Input hashes</h3>
  {#if inputs.length}<ul>
      {#each inputs as input, index (index)}<li>
          {formatRecordedValue(asObject(input)?.parameter)}: {formatRecordedValue(
            asObject(input)?.sha256,
          )}
        </li>{/each}
    </ul>{:else}<p>
      {manifestReady
        ? 'No input hashes recorded.'
        : 'Input hashes are not available.'}
    </p>{/if}
  <h3>Resolved workflow</h3>
  <details bind:open={showResolved}>
    <summary>Show resolved workflow JSON</summary>
    <p>
      The graph comes from recorded ComfyUI history when available; the archived
      fallback is unverified.
    </p>
    {#if resolvedLoading}<p role="status">Loading workflow…</p>{/if}
    {#if resolvedError}<ProblemDetails problem={resolvedError} />{/if}
    {#if showResolved && resolved !== undefined}<pre>{formatRecordedValue(
          resolved,
        )}</pre>{/if}
  </details>
  <h3>Known re-runs</h3>
  {#if relatives.length}<ul>
      {#each relatives as relative (relative.id)}<li>
          <a href={path(relative.id)}>{relative.id}</a>
        </li>{/each}
    </ul>{:else}<p>No re-runs found in recent jobs.</p>{/if}
  {#if moreRelatives}<p>
      Only recent jobs were checked. <a href="#/jobs">Browse Jobs</a>.
    </p>{/if}
  <h2>Outputs</h2>
  {#if outputs.length}<ul class="outputs">
      {#each outputs as output (output.id)}<li>
          <a href={gallery(output.id)}
            ><span class="thumb"><Thumbnail {output} /></span
            >{output.source_filename}</a
          >
          <p>SHA-256: {output.sha256}</p>
        </li>{/each}
    </ul>{:else}<p>No indexed outputs are available.</p>{/if}
  {#if outputCursor}<Button onclick={moreOutputPage} disabled={loadingOutputs}
      >Load more outputs</Button
    >{/if}
  <h2>Event timeline</h2>
  {#if timeline.length === 500}<p>
      Showing the latest 500 recorded events.
    </p>{/if}
  {#if timeline.length}<ol>
      {#each timeline as event (event.id)}<li>
          <time datetime={event.created_at}>{event.created_at}</time> — {event.event_type}{#if event.message}:
            {event.message}{/if}
        </li>{/each}
    </ol>{:else}<p>No recorded events available.</p>{/if}
  <h2>Actions</h2>
  {#if run.status === 'unknown_submission'}<p>
      This submission is uncertain. <a href="#/jobs">Reconcile in Jobs</a>
      before submitting a new run.
      {#if pendingKey}A previous re-run request is also uncertain; do not start
        a new attempt until it is resolved.{/if}
    </p>
  {:else if !storageAvailable}<p>
      Re-run is unavailable because browser session storage could not be read.
      Check <a href="#/jobs">Jobs</a> before taking further action.
    </p>
  {:else if pendingKey}<p>
      The previous re-run was not confirmed. Check <a href="#/jobs">Jobs</a> before
      retrying with the same key.
    </p>
    <Button onclick={() => submit(true)} disabled={busy}
      >Retry same request</Button
    >
  {:else if !manifestReady}<p>
      Re-run requires a verified manifest matching the run and exactly
      displayable recorded parameters to review the preset and inputs. See <a
        href="#/jobs">Jobs</a
      >.
    </p>
  {:else}<Button onclick={() => (open = true)} disabled={busy}>Re-run</Button
    >{/if}
  {#if rerunUrl}<a href={rerunUrl}>Remix in Generate</a>
    <p>
      Only the workflow is preselected. Recorded parameters are not yet carried
      across; review them before generating.
    </p>
  {/if}
  {#if manifestText}<Button onclick={download}>Download manifest</Button
    >{:else if manifestError}<p>Run manifest is unavailable.</p>{/if}
  {#if actionNotice}<p role="status">{actionNotice}</p>{/if}
  {#if actionError}<ProblemDetails problem={actionError} />{/if}
  {#if mismatch && rerunUrl}<p>
      Use <a href={rerunUrl}>Remix</a> to review the current workflow instead.
    </p>{/if}
  <Dialog bind:open title="Confirm re-run" returnFocus={() => heading.focus()}>
    <p>
      A new run will reuse the recorded workflow, version, preset, parameters,
      seed and input files after verification. The original run will remain
      unchanged. Identical output is not guaranteed.
    </p>
    <p><strong>Workflow:</strong> {workflowId} ({workflowVersion})</p>
    <p><strong>Workflow SHA-256:</strong> {workflowHash}</p>
    {#if workflow?.manifest_sha256}<p>
        <strong>Workflow manifest SHA-256:</strong>
        {formatRecordedValue(workflow.manifest_sha256)}
      </p>{/if}
    {#if manifest && 'preset' in manifest}<p>
        <strong>Preset:</strong>
        {manifest.preset === null
          ? 'None'
          : formatRecordedValue(manifest.preset)}
      </p>{:else}<p>
        Preset and input details are not available in the run API until a
        manifest is recorded.
      </p>{/if}
    <p><strong>Parameters:</strong> {formatRecordedValue(parameters)}</p>
    {#if inputs.length}<p><strong>Input hashes:</strong></p>
      <ul>
        {#each inputs as input, index (index)}<li>
            {formatRecordedValue(asObject(input)?.parameter)}: {formatRecordedValue(
              asObject(input)?.sha256,
            )}
          </li>{/each}
      </ul>{/if}
    <Button variant="primary" onclick={() => submit()} disabled={busy}
      >Confirm re-run</Button
    >
  </Dialog>
{/if}

<style>
  dl {
    display: grid;
    gap: var(--space-2);
  }
  dt,
  .muted {
    color: var(--muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .outputs {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
    gap: var(--space-3);
    padding: 0;
    list-style: none;
  }
  .outputs li {
    overflow-wrap: anywhere;
  }
  .thumb {
    display: block;
    height: 10rem;
  }
</style>
