<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import {
    api,
    ApiProblemError,
    outputThumbnailUrl,
  } from '../../lib/api/client';
  import { events } from '../../lib/api/events';
  import type { ConnectionStatus, EventType } from '../../lib/api/events';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';

  let { params: _params, query: _query }: RouteProps = $props();
  type Health = components['schemas']['HealthResponse'];
  type Run = components['schemas']['RunResponse'];
  type Output = components['schemas']['OutputResponse'];
  type Status = components['schemas']['RunStatus'];
  type HealthObservation = 'ok' | 'unavailable' | 'unknown';
  const runLimit = 50;
  const healthEventInterval = 5_000;
  const announcementInterval = 400;
  const statuses: { value: Status; label: string }[] = [
    { value: 'created', label: 'Created' },
    { value: 'submitting', label: 'Submitting' },
    { value: 'unknown_submission', label: 'Needs reconciliation' },
    { value: 'queued', label: 'Queued' },
    { value: 'running', label: 'Running' },
    { value: 'completed', label: 'Completed' },
    { value: 'failed', label: 'Failed' },
    { value: 'cancelling', label: 'Cancelling' },
    { value: 'cancelled', label: 'Cancelled' },
  ];
  const healthLabels: Record<Health['self'], string> = {
    ok: 'OK',
    unavailable: 'Unavailable',
    not_configured: 'Not configured',
  };
  let health = $state<Health | null>(null);
  let runs = $state<Run[] | null>(null);
  let outputs = $state<Output[] | null>(null);
  let healthError = $state<Problem | null>(null);
  let runsError = $state<Problem | null>(null);
  let outputsError = $state<Problem | null>(null);
  let connection = $state<ConnectionStatus>('connecting');
  let runsTruncated = $state(false);
  let brokenThumbnails = $state<string[]>([]);
  let announcement = $state('');
  let counts = $derived(
    statuses.map((status) => ({
      ...status,
      count: runs?.filter((run) => run.status === status.value).length ?? 0,
    })),
  );
  let recent = $derived(
    (runs ?? [])
      .slice()
      .sort(
        (a, b) =>
          compareInstant(b.created_at, a.created_at) ||
          b.id.localeCompare(a.id),
      )
      .slice(0, 6),
  );

  function problem(error: unknown): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title: 'Unable to load dashboard data',
      detail: error instanceof Error ? error.message : 'The request failed.',
      recovery:
        'Check that the local GenAImage server is running, then try again.',
    };
  }
  function statusLabel(status: Status) {
    return statuses.find((item) => item.value === status)?.label ?? status;
  }
  function compareInstant(a: string, b: string): number {
    const difference = Date.parse(a) - Date.parse(b);
    if (Number.isNaN(difference)) return a.localeCompare(b);
    if (difference) return difference;
    const submillis = (value: string) =>
      Number((value.match(/\.(\d+)/)?.[1] ?? '').padEnd(9, '0').slice(3, 9));
    return submillis(a) - submillis(b);
  }
  function newestRuns(list: Run[]): Run[] {
    return list
      .slice()
      .sort(
        (a, b) =>
          compareInstant(b.created_at, a.created_at) ||
          b.id.localeCompare(a.id),
      )
      .slice(0, runLimit);
  }
  const pendingAnnouncements = new SvelteMap<string, string>();
  let announcementOverflow = 0;
  let announcementTimer: ReturnType<typeof setTimeout> | undefined;
  function queueAnnouncement(key: string, message: string) {
    if (
      pendingAnnouncements.has(key) ||
      pendingAnnouncements.size < runLimit + 3
    )
      pendingAnnouncements.set(key, message);
    else announcementOverflow++;
    announcementTimer ??= setTimeout(async () => {
      announcementTimer = undefined;
      const messages = [...pendingAnnouncements.values()];
      const remaining = Math.max(0, messages.length - 2) + announcementOverflow;
      const text = remaining
        ? `${messages.slice(0, 2).join(' ')} ${remaining} more ${remaining === 1 ? 'update' : 'updates'}. Check Jobs and service health.`
        : messages.join(' ');
      pendingAnnouncements.clear();
      announcementOverflow = 0;
      announcement = '';
      await tick();
      if (active) announcement = text;
    }, announcementInterval);
  }
  function announceRun(run: Run, previous?: Run) {
    if (previous?.status === run.status) return;
    const labels: Partial<Record<Status, string>> = {
      completed: 'completed',
      failed: 'failed',
      cancelled: 'cancelled',
      unknown_submission: 'needs reconciliation. Go to Jobs',
    };
    const label = labels[run.status];
    const key = `run:${run.id}`;
    if (label) queueAnnouncement(key, `Run ${run.id} ${label}.`);
    else {
      pendingAnnouncements.delete(key);
      if (!pendingAnnouncements.size && !announcementOverflow) {
        clearTimeout(announcementTimer);
        announcementTimer = undefined;
      }
    }
  }
  function announceHealth(previous: Health | null, current: Health) {
    if (!previous) return;
    const services = [
      { name: 'GenAImage', before: previous.self, after: current.self },
      {
        name: 'ComfyUI',
        before: previous.image_backend,
        after: current.image_backend,
      },
      { name: 'LLM', before: previous.llm, after: current.llm },
    ];
    for (const service of services) {
      if (service.before === service.after) continue;
      const labels: Record<Health['self'], string> = {
        ok: 'available again',
        unavailable: 'unavailable',
        not_configured: 'not configured',
      };
      const label = labels[service.after] ?? service.after;
      queueAnnouncement(
        `health:${service.name}`,
        `${service.name} is ${label}.`,
      );
    }
  }
  function mergeRun(list: Run[], run: Run, allowEqual = false): Run[] {
    const existing = list.find((item) => item.id === run.id);
    if (existing) {
      const order = compareInstant(existing.updated_at, run.updated_at);
      if (order > 0 || (!allowEqual && order === 0)) return list;
    }
    return [...list.filter((item) => item.id !== run.id), run];
  }

  let active = false;
  let healthController: AbortController | undefined;
  let runController: AbortController | undefined;
  let outputController: AbortController | undefined;
  let runBuffer: Map<string, Run> | null = null;
  let outputDirty = false;
  let bufferOverflow = false;
  let pendingHealth: HealthObservation | null = null;
  let healthTimer: ReturnType<typeof setTimeout> | undefined;
  let lastProbeStart = -Infinity;

  function matchesHealth(observation: HealthObservation): boolean {
    if (!health || healthError || observation === 'unknown') return false;
    if (observation === 'ok') return health.image_backend === 'ok';
    return (
      health.image_backend === 'unavailable' &&
      health.image_backend_reason === 'probe_failed'
    );
  }
  function scheduleHealthRefresh() {
    if (!active || !pendingHealth || healthController || healthTimer) return;
    healthTimer = setTimeout(
      () => {
        healthTimer = undefined;
        pendingHealth = null;
        void loadHealth();
      },
      Math.max(0, healthEventInterval - (Date.now() - lastProbeStart)),
    );
  }
  function healthChanged(observation: HealthObservation) {
    if (!healthController && matchesHealth(observation)) {
      pendingHealth = null;
      clearTimeout(healthTimer);
      healthTimer = undefined;
      return;
    }
    pendingHealth = observation;
    scheduleHealthRefresh();
  }

  async function loadHealth(force = false) {
    if (!active) return;
    if (healthController && !force) return;
    healthController?.abort();
    clearTimeout(healthTimer);
    healthTimer = undefined;
    const controller = new AbortController();
    healthController = controller;
    lastProbeStart = Date.now();
    try {
      const result = await api.health({ signal: controller.signal });
      if (!active || controller.signal.aborted) return;
      announceHealth(health, result);
      health = result;
      healthError = null;
      if (pendingHealth && matchesHealth(pendingHealth)) pendingHealth = null;
    } catch (error) {
      if (!active || controller.signal.aborted) return;
      healthError = problem(error);
    } finally {
      if (healthController === controller) {
        healthController = undefined;
        scheduleHealthRefresh();
      }
    }
  }
  async function loadRuns() {
    if (!active) return;
    runController?.abort();
    const controller = new AbortController();
    runController = controller;
    const buffered = new SvelteMap<string, Run>();
    runBuffer = buffered;
    bufferOverflow = false;
    try {
      const page = await api.listRuns(
        { limit: runLimit },
        { signal: controller.signal },
      );
      if (!active || controller.signal.aborted) return;
      let list = page.runs;
      for (const run of buffered.values()) list = mergeRun(list, run);
      runsTruncated =
        Boolean(page.next_cursor) || bufferOverflow || list.length > runLimit;
      const next = newestRuns(list);
      if (runs) {
        for (const run of next) {
          const previous = runs.find((item) => item.id === run.id);
          if (
            !previous ||
            compareInstant(previous.updated_at, run.updated_at) <= 0
          )
            announceRun(run, previous);
        }
      }
      runs = next;
      runsError = null;
    } catch (error) {
      if (!active || controller.signal.aborted) return;
      runsError = problem(error);
    } finally {
      if (runController === controller) {
        runController = undefined;
        runBuffer = null;
      }
    }
  }
  async function loadOutputs(force = false) {
    if (!active) return;
    if (outputController && !force) {
      outputDirty = true;
      return;
    }
    outputController?.abort();
    outputDirty = false;
    const controller = new AbortController();
    outputController = controller;
    try {
      const result = await api.listOutputs(
        { limit: 6 },
        { signal: controller.signal },
      );
      if (!active || controller.signal.aborted) return;
      outputs = result.outputs;
      brokenThumbnails = brokenThumbnails.filter((id) =>
        result.outputs.some((output) => output.id === id),
      );
      outputsError = null;
    } catch (error) {
      if (!active || controller.signal.aborted) return;
      outputsError = problem(error);
    } finally {
      if (outputController === controller) {
        outputController = undefined;
        if (active && outputDirty) {
          outputDirty = false;
          void loadOutputs();
        }
      }
    }
  }
  function refresh() {
    void loadHealth(true);
    void loadRuns();
    void loadOutputs(true);
  }

  onMount(() => {
    active = true;
    const runTypes = [
      'run_queued',
      'run_started',
      'run_progress',
      'run_completed',
      'run_failed',
      'run_cancelled',
      'run_unknown_submission',
    ] as const satisfies readonly EventType[];
    const stops = runTypes.map((type) =>
      events.on(type, (event) => {
        if (runBuffer) {
          const previous = runBuffer.get(event.run.id);
          if (
            !previous ||
            compareInstant(previous.updated_at, event.run.updated_at) <= 0
          ) {
            runBuffer.set(event.run.id, event.run);
            if (runBuffer.size > runLimit) {
              const retained = newestRuns([...runBuffer.values()]);
              const ids = new Set(retained.map((run) => run.id));
              for (const id of runBuffer.keys())
                if (!ids.has(id)) runBuffer.delete(id);
              bufferOverflow = true;
            }
          }
        } else if (runs) {
          const previous = runs.find((run) => run.id === event.run.id);
          const merged = mergeRun(runs, event.run, true);
          if (merged !== runs) {
            if (merged.length > runLimit) runsTruncated = true;
            runs = newestRuns(merged);
            if (runs.some((run) => run.id === event.run.id))
              announceRun(event.run, previous);
          }
        }
        if (event.type === 'run_completed') void loadOutputs();
      }),
    );
    stops.push(events.on('run_output', () => void loadOutputs()));
    // Health events are published by probes, not a background monitor. Poll while visible as well.
    stops.push(
      events.on('backend_health_changed', (event) => {
        healthChanged(
          event.health && !event.error
            ? 'ok'
            : !event.health && event.error
              ? 'unavailable'
              : 'unknown',
        );
      }),
    );
    let opened = false;
    stops.push(
      events.status.subscribe((status) => {
        connection = status;
        if (status === 'open' && !opened) {
          opened = true;
          refresh();
        }
      }),
    );
    let epoch = 0;
    stops.push(
      events.resync.subscribe((signal) => {
        if (signal && signal.epoch !== epoch) {
          epoch = signal.epoch;
          refresh();
        }
      }),
    );
    refresh();
    const timer = window.setInterval(() => {
      if (!document.hidden) void loadHealth();
    }, 30_000);
    const visible = () => {
      if (!document.hidden) void loadHealth();
    };
    document.addEventListener('visibilitychange', visible);
    return () => {
      active = false;
      healthController?.abort();
      runController?.abort();
      outputController?.abort();
      window.clearInterval(timer);
      clearTimeout(healthTimer);
      clearTimeout(announcementTimer);
      pendingAnnouncements.clear();
      document.removeEventListener('visibilitychange', visible);
      stops.forEach((stop) => stop());
    };
  });
</script>

<h1>Dashboard</h1>
<p class="intro">Service health and recent activity at a glance.</p>
<p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
  {announcement}
</p>
<p class="connection" role="status">
  {#if connection === 'open'}Live updates connected
  {:else if connection === 'connecting'}Connecting to live updates…
  {:else}Live updates offline; reconnecting. Displayed activity may be out of
    date.{/if}
</p>

<section aria-labelledby="health-heading">
  <div class="section-heading">
    <h2 id="health-heading">Service health</h2>
    <Button onclick={() => void loadHealth()}>Refresh health</Button>
  </div>
  {#if healthError}<ProblemDetails problem={healthError} />
  {:else if !health}<p role="status">Checking services…</p>
  {:else}
    <div class="health-grid">
      {#each [{ name: 'GenAImage', status: health.self, hint: health.hints?.self }, { name: 'ComfyUI', status: health.image_backend, hint: health.hints?.image_backend }, { name: 'LLM', status: health.llm, hint: health.hints?.llm }] as service (service.name)}
        <article class="card">
          <h3>{service.name}</h3>
          <p class="health-status">
            {healthLabels[service.status] ?? service.status}
          </p>
          {#if service.status !== 'ok' && service.hint}<p>
              {service.hint}
            </p>{/if}
          {#if service.name === 'ComfyUI' && health.image_backend_reason === 'timeout'}<p
            >
              The health probe timed out.
            </p>
          {:else if service.name === 'ComfyUI' && health.image_backend_reason === 'probe_failed'}<p
            >
              The health probe failed.
            </p>{/if}
        </article>
      {/each}
    </div>
  {/if}
</section>

<section aria-labelledby="runs-heading">
  <div class="section-heading">
    <h2 id="runs-heading">Queue and recent runs</h2>
    <a href="#/jobs">View jobs</a>
  </div>
  {#if runsError}<ProblemDetails problem={runsError} /><Button
      onclick={() => void loadRuns()}>Retry runs</Button
    >
  {:else if !runs}<p role="status">Loading runs…</p>
  {:else if !runs.length}<p>
      No runs yet. <a href="#/generate">Go to Generate</a> to get started.
    </p>
  {:else}
    <p class="muted">
      Counts cover the latest {runLimit} recorded runs{runsTruncated
        ? '; + indicates a lower bound'
        : ''}.
    </p>
    <dl class="counts">
      {#each counts as item (item.value)}<div>
          <dt>{item.label}</dt>
          <dd>{item.count}{runsTruncated ? '+' : ''}</dd>
        </div>{/each}
    </dl>
    {#if counts.find((item) => item.value === 'unknown_submission')?.count}<p
        class="notice"
      >
        Unknown submissions need reconciliation before any retry. <a
          href="#/jobs">Go to Jobs</a
        >.
      </p>{/if}
    <h3>Recent runs</h3>
    <ul class="run-list">
      {#each recent as run (run.id)}<li>
          <div>
            <a href={`#/runs/${encodeURIComponent(run.id)}`}
              >{run.workflow_id}</a
            >
            <p class="muted">Run {run.id}</p>
          </div>
          <strong>{statusLabel(run.status)}</strong>
        </li>{/each}
    </ul>
  {/if}
</section>

<section aria-labelledby="outputs-heading">
  <div class="section-heading">
    <h2 id="outputs-heading">Recent outputs</h2>
    <a href="#/gallery">View gallery</a>
  </div>
  {#if outputsError}<ProblemDetails problem={outputsError} /><Button
      onclick={() => void loadOutputs()}>Retry outputs</Button
    >
  {:else if !outputs}<p role="status">Loading outputs…</p>
  {:else if !outputs.length}<p>No outputs yet.</p>
  {:else}<ul class="outputs">
      {#each outputs as output (output.id)}<li>
          <!-- Gallery #47 will select this output from the query parameter. -->
          <a
            href={`#/gallery?output=${encodeURIComponent(output.id)}`}
            aria-label={`View ${output.source_filename} in Gallery`}
          >
            {#if ['image/png', 'image/jpeg', 'image/webp'].includes(output.content_type) && !brokenThumbnails.includes(output.id)}
              <img
                src={outputThumbnailUrl(output.id, 256)}
                alt=""
                loading="lazy"
                onerror={() => {
                  brokenThumbnails = [...brokenThumbnails, output.id];
                }}
              />
            {:else}
              <span class="media-fallback"
                >Thumbnail unavailable ({output.content_type})</span
              >
            {/if}
            <span>{output.source_filename}</span>
          </a>
        </li>{/each}
    </ul>{/if}
</section>

<section aria-labelledby="quick-heading">
  <h2 id="quick-heading">Quick links</h2>
  <p>
    <a class="quick-link" href="#/generate">Go to Generate</a>
    <a class="quick-link" href="#/gallery">Browse Gallery</a>
  </p>
</section>

<style>
  .intro,
  .muted {
    color: var(--muted);
  }
  .connection {
    margin-block: var(--space-4);
    padding: var(--space-3);
    background: var(--subtle);
    border-radius: var(--radius-control);
  }
  section {
    margin-block-start: var(--space-6);
  }
  .section-heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    margin-block-end: var(--space-3);
  }
  .health-grid,
  .counts,
  .outputs {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 12rem), 1fr));
    gap: var(--space-3);
  }
  .card,
  .counts div {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding: var(--space-3);
  }
  h3 {
    margin: 0 0 var(--space-2);
  }
  .health-status {
    font-weight: var(--weight-strong);
  }
  .counts {
    margin-block: var(--space-3);
  }
  .counts dd {
    margin: 0;
    font-size: 1.4rem;
    font-weight: var(--weight-strong);
    font-variant-numeric: tabular-nums;
  }
  .notice {
    padding: var(--space-3);
    background: var(--subtle);
    margin-block: var(--space-3);
  }
  .run-list,
  .outputs {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .run-list li {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: var(--space-2);
    border-block-start: 1px solid var(--border);
    padding-block: var(--space-3);
  }
  .outputs img,
  .media-fallback {
    display: block;
    width: 100%;
    aspect-ratio: 1;
    object-fit: contain;
    background: var(--subtle);
    border-radius: var(--radius-control);
  }
  .media-fallback {
    display: grid;
    place-items: center;
    padding: var(--space-3);
    color: var(--ink);
    text-align: center;
  }
  .outputs a {
    display: block;
  }
  .quick-link {
    display: inline-block;
    padding: var(--space-2) var(--space-3);
    margin: var(--space-2) var(--space-2) 0 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
  }
</style>
