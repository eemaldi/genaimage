<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import { events } from '../../lib/api/events';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import { keepFocusAcross } from '../../lib/ui/focus';
  import StatusBadge from '../jobs/StatusBadge.svelte';
  import BatchOutputs from './BatchOutputs.svelte';
  import {
    canResume,
    describeSweep,
    needsReconcile,
    stateLabels,
    submissionLabels,
    sweepLabel,
  } from './labels';
  import { createBatchPoller, type Batch } from './poller';

  let { id: batchId }: { id: string } = $props();
  const id = untrack(() => batchId);
  const pageSize = 50;
  const runEvents = [
    'run_queued',
    'run_started',
    'run_progress',
    'run_output',
    'run_completed',
    'run_failed',
    'run_cancelled',
    'run_unknown_submission',
  ] as const;
  const connection = events.status;
  const date = new Intl.DateTimeFormat('en-GB', {
    dateStyle: 'medium',
    timeStyle: 'medium',
  });

  let batch = $state.raw<Batch>();
  let loadError = $state.raw<unknown>();
  let notice = $state('');
  let resumeProblem = $state<Problem>();
  let resuming = $state(false);
  let page = $state(0);
  let heading = $state<HTMLElement>();
  let connected = false;
  const unsubscribe = connection.subscribe((status) => {
    connected = status === 'open';
  });

  const poller = createBatchPoller({
    client: api,
    id,
    onBatch(next) {
      batch = next;
      loadError = undefined;
    },
    onError(error) {
      loadError = error;
    },
    live: () => connected,
  });
  void poller.refresh();
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  const disposers = runEvents.map((type) =>
    events.on(type, (event) => {
      if (!('run' in event) || event.run.batch_id !== id) return;
      clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => void poller.refresh(), 400);
    }),
  );
  onDestroy(() => {
    poller.stop();
    clearTimeout(refreshTimer);
    unsubscribe();
    disposers.forEach((dispose) => dispose());
  });

  let pages = $derived(
    batch ? Math.max(1, Math.ceil(batch.items.length / pageSize)) : 1,
  );
  let shown = $derived(
    batch?.items.slice(page * pageSize, (page + 1) * pageSize) ?? [],
  );
  const runHref = (runId: string) => `#/runs/${encodeURIComponent(runId)}`;
  const time = (value: string) =>
    Number.isFinite(Date.parse(value)) ? date.format(new Date(value)) : value;

  function problemOf(error: unknown, title: string): Problem {
    return error instanceof ApiProblemError
      ? error.problem
      : {
          title,
          detail: error instanceof Error ? error.message : undefined,
          recovery:
            'Check the latest batch state before taking further action.',
        };
  }

  async function resume() {
    if (resuming || !batch || !canResume(batch)) return;
    resuming = true;
    resumeProblem = undefined;
    const button = document.getElementById('batch-resume');
    const restoreFocus = button && keepFocusAcross(button);
    try {
      const next = await api.resumeBatch(id);
      poller.accept(next);
      notice =
        next.state === 'submitting'
          ? 'Resumed. Runs that were not sent are being submitted.'
          : 'Resume finished. The current state is shown below.';
    } catch (error) {
      resumeProblem = problemOf(error, 'Could not resume the batch');
      void poller.refresh();
    } finally {
      resuming = false;
      await tick();
      await restoreFocus?.();
      if (!document.activeElement || document.activeElement === document.body)
        heading?.focus();
    }
  }
</script>

<h1 tabindex="-1" bind:this={heading}>Batch</h1>
<p><a href="#/jobs?view=batches">All batches</a></p>
{#if loadError && !batch}
  <ProblemDetails problem={problemOf(loadError, 'Could not load the batch')} />
  <Button onclick={() => poller.refresh()}>Reload batch</Button>
{:else if !batch}
  <p role="status">Loading batch…</p>
{:else}
  {@const info = stateLabels[batch.state]}
  <p><code>{batch.id}</code></p>
  <section aria-labelledby="batch-state">
    <h2 id="batch-state">Submission</h2>
    <p role="status" aria-live="polite">
      <strong>{info.label}.</strong>
      {info.detail}
    </p>
    {#if loadError}
      <ProblemDetails
        problem={problemOf(loadError, 'Could not refresh the batch')}
      />
      <Button onclick={() => poller.refresh()}>Reload batch</Button>
    {/if}
    <p class="muted">
      {$connection === 'open'
        ? 'Live run updates connected.'
        : 'Live run updates are not connected.'}
    </p>
    <dl>
      <div>
        <dt>Workflow</dt>
        <dd>{batch.workflow}</dd>
      </div>
      <div>
        <dt>Revision</dt>
        <dd><code>{batch.workflow_revision}</code></dd>
      </div>
      <div>
        <dt>Created</dt>
        <dd>
          <time datetime={batch.created_at}>{time(batch.created_at)}</time>
        </dd>
      </div>
      <div>
        <dt>Sweep</dt>
        <dd>{describeSweep(batch.sweep)}</dd>
      </div>
      <div>
        <dt>Runs</dt>
        <dd>{batch.item_count}</dd>
      </div>
    </dl>
    <h3>Submission counts</h3>
    <ul class="counts" aria-label="Submission counts">
      <li>Sent: {batch.counts.submitted}</li>
      <li>Waiting: {batch.counts.pending}</li>
      <li>Not sent: {batch.counts.not_submitted}</li>
      <li>Rejected: {batch.counts.failed}</li>
      <li>Outcome unknown: {batch.counts.outcome_unknown}</li>
    </ul>
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if resumeProblem}<ProblemDetails problem={resumeProblem} />{/if}
    {#if canResume(batch)}
      <p>
        Resuming sends only runs that have no run yet. Runs that already exist
        are never sent again.
      </p>
      <Button id="batch-resume" onclick={resume} disabled={resuming}
        >Resume submission</Button
      >
    {/if}
    {#if batch.counts.outcome_unknown}
      <p>
        Some runs may or may not have reached ComfyUI. GenAImage will not send
        them again. Reconcile each one in
        <a
          href={`#/jobs?batch=${encodeURIComponent(batch.id)}&status=unknown_submission`}
          >Jobs</a
        >.
      </p>
    {/if}
  </section>
  <section aria-labelledby="batch-runs">
    <h2 id="batch-runs">Runs</h2>
    <table>
      <caption>
        Runs {page * pageSize + 1} to {page * pageSize + shown.length} of {batch
          .items.length}. “Sent” means ComfyUI was given the run, not that its
        images exist.
      </caption>
      <thead>
        <tr>
          <th scope="col">Run</th>
          <th scope="col">Values</th>
          <th scope="col">Submission</th>
          <th scope="col">Run status</th>
          <th scope="col">Details</th>
        </tr>
      </thead>
      <tbody>
        {#each shown as item (item.index)}
          <tr>
            <th scope="row">{item.index + 1}</th>
            <td>{sweepLabel(item.sweep_values)}</td>
            <td>
              {submissionLabels[item.submission]}
              {#if item.submission === 'failed' && !item.run_id}
                <span class="muted">(can be resumed)</span>
              {/if}
            </td>
            <td>
              {#if item.run_status}<StatusBadge status={item.run_status} />
              {:else}<span class="muted">No run yet</span>{/if}
            </td>
            <td>
              {#if item.error}<span>{item.error}</span>{/if}
              {#if item.run_id}
                <a href={runHref(item.run_id)}>View run</a>
              {/if}
              {#if needsReconcile(item)}
                <a
                  href={`#/jobs?batch=${encodeURIComponent(batch.id)}&status=unknown_submission`}
                  >Reconcile in Jobs</a
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if pages > 1}
      <nav aria-label="Run pages" class="pager">
        <Button disabled={page === 0} onclick={() => page--}
          >Previous runs</Button
        >
        <span>Page {page + 1} of {pages}</span>
        <Button disabled={page >= pages - 1} onclick={() => page++}
          >Next runs</Button
        >
      </nav>
    {/if}
  </section>
  <BatchOutputs {batch} />
{/if}

<style>
  dl {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  dd {
    margin: 0;
  }
  dt,
  .muted {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    list-style: none;
    padding: 0;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    text-align: left;
  }
  caption {
    text-align: left;
    padding-bottom: var(--space-2);
  }
  th,
  td {
    padding: var(--space-2);
    border-bottom: 1px solid var(--border);
    vertical-align: top;
    overflow-wrap: anywhere;
  }
  td a {
    display: block;
  }
  .pager {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
</style>
