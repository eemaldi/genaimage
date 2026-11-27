<script lang="ts">
  import { untrack } from 'svelte';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import { api, ApiProblemError } from '../../lib/api/client';
  import { events } from '../../lib/api/events';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { components } from '../../lib/api/schema';
  import Thumbnail from '../gallery/Thumbnail.svelte';
  import StatusBadge from '../jobs/StatusBadge.svelte';
  import { compareTime, mergeRunEvent, type RunRow } from '../jobs/run-list';

  type Run = components['schemas']['RunResponse'];
  type Output = components['schemas']['OutputResponse'];
  let {
    initial,
    onconfirmed,
  }: { initial: Run; onconfirmed?: (runId: string) => void } = $props();
  let row = $state.raw<RunRow>(untrack(() => ({ run: initial })));
  let outputs = $state.raw<Output[]>();
  let snapshotProblem = $state<Problem>();
  let outputsProblem = $state<Problem>();
  let reconnecting = $state(false);
  const uid = $props.id();
  let status = $derived(row.run.status);
  $effect(() => {
    if (status === 'completed' && outputs?.length)
      untrack(() => onconfirmed?.(row.run.id));
  });
  const runEvents = [
    'run_queued',
    'run_started',
    'run_progress',
    'run_completed',
    'run_failed',
    'run_cancelled',
    'run_unknown_submission',
  ] as const;

  $effect(() => {
    const id = initial.id;
    const session = new AbortController();
    let snapshotRequest: AbortController | undefined;
    let outputRequest: AbortController | undefined;
    let epoch: number | undefined;
    let wasOpen = false;

    function problem(failure: unknown, title: string): Problem {
      return failure instanceof ApiProblemError
        ? failure.problem
        : {
            title,
            detail: failure instanceof Error ? failure.message : undefined,
          };
    }

    async function readOutputs() {
      outputRequest?.abort();
      const controller = new AbortController();
      outputRequest = controller;
      outputsProblem = undefined;
      try {
        const found = new SvelteMap<string, Output>();
        const seen = new SvelteSet<string>();
        let cursor: string | undefined;
        do {
          const page = await api.listOutputs(
            { run: id, status: 'completed', cursor },
            { signal: controller.signal },
          );
          if (controller.signal.aborted) return;
          for (const output of page.outputs)
            if (output.run_id === id) found.set(output.id, output);
          cursor = page.next_cursor ?? undefined;
          if (cursor && seen.has(cursor))
            throw new Error('The output list could not be paged.');
          if (cursor) seen.add(cursor);
        } while (cursor);
        if (row.run.status === 'completed') outputs = [...found.values()];
      } catch (failure) {
        if (!controller.signal.aborted)
          outputsProblem = problem(failure, 'Could not load the run outputs');
      }
    }

    async function refresh() {
      snapshotRequest?.abort();
      const controller = new AbortController();
      snapshotRequest = controller;
      snapshotProblem = undefined;
      const baseline = row;
      try {
        const run = await api.getRun(id, { signal: controller.signal });
        if (controller.signal.aborted) return;
        const order = compareTime(run.updated_at, row.run.updated_at);
        if (
          order > 0 ||
          (order === 0 && row === baseline && run.status !== row.run.status)
        )
          row = { run };
        if (row.run.status === 'completed') void readOutputs();
      } catch (failure) {
        if (!controller.signal.aborted)
          snapshotProblem = problem(failure, 'Could not refresh the run');
      }
    }

    const unsubscribe = untrack(() => {
      const listeners = runEvents.map((type) =>
        events.on(type, (event) => {
          if (event.run.id !== id || session.signal.aborted) return;
          const previous = row.run.status;
          row = mergeRunEvent(row, event);
          if (row.run.status === 'completed' && previous !== 'completed')
            void readOutputs();
        }),
      );
      listeners.push(
        events.on('run_output', (event) => {
          if (
            event.run_id === id &&
            row.run.status === 'completed' &&
            !session.signal.aborted
          )
            void readOutputs();
        }),
      );
      listeners.push(
        events.status.subscribe((connection) => {
          reconnecting = connection !== 'open';
          if (connection === 'open' && !wasOpen) {
            wasOpen = true;
            void refresh();
          }
        }),
      );
      listeners.push(
        events.resync.subscribe((resync) => {
          if (!resync || epoch === resync.epoch) return;
          epoch = resync.epoch;
          // The replacement stream is open. Old transient progress is not durable.
          row = { run: row.run };
          void refresh();
        }),
      );
      if (!wasOpen) void refresh();
      return listeners;
    });
    return () => {
      session.abort();
      snapshotRequest?.abort();
      outputRequest?.abort();
      for (const off of unsubscribe) off();
    };
  });
</script>

<section aria-labelledby={`${uid}-run`}>
  <h3 id={`${uid}-run`}>Run progress</h3>
  <p>
    <a href={`#/runs/${encodeURIComponent(row.run.id)}`}>View run details</a>
  </p>
  <p role="status">
    {#if status === 'completed' && !outputs?.length}Run ended{:else}<StatusBadge
        {status}
      />{/if}
    {#if row.node}
      · Current node: {row.node}{/if}
    {#if reconnecting}
      · Waiting for live updates{/if}
  </p>
  {#if row.progress}
    <progress
      aria-label="Run progress"
      value={row.progress.value}
      max={row.progress.max}
    ></progress>
    <span>{row.progress.value} / {row.progress.max}</span>
  {/if}
  {#if status === 'unknown_submission'}
    <p>
      The backend submission is uncertain. <a
        href="#/jobs?status=unknown_submission">Reconcile in Jobs</a
      > before starting another run.
    </p>
  {/if}
  {#if row.run.error}<p>{row.run.error}</p>{/if}
  {#if snapshotProblem}<ProblemDetails problem={snapshotProblem} />{/if}
  {#if status === 'completed'}
    {#if outputs?.length}
      <p role="status">Run completed with outputs.</p>
      <ul>
        {#each outputs as output (output.id)}
          <li>
            <a href={`#/gallery?${new URLSearchParams({ output: output.id })}`}>
              <span class="preview"><Thumbnail {output} /></span>
              {output.source_filename}
            </a>
          </li>
        {/each}
      </ul>
    {:else if outputsProblem}
      <ProblemDetails problem={outputsProblem} />
    {:else if outputs}
      <p role="status">No outputs were reported for this run.</p>
    {:else}
      <p role="status">Checking for outputs…</p>
    {/if}
  {/if}
</section>

<style>
  ul {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr));
    gap: 1rem;
    padding: 0;
    list-style: none;
  }
  li {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .preview {
    display: block;
    aspect-ratio: 4 / 3;
  }
</style>
