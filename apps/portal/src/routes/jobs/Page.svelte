<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    SvelteMap,
    SvelteSet,
    SvelteURLSearchParams,
  } from 'svelte/reactivity';
  import { api, ApiProblemError } from '../../lib/api/client';
  import { events } from '../../lib/api/events';
  import Button from '../../lib/ui/Button.svelte';
  import Dialog from '../../lib/ui/Dialog.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import { navigateHash } from '../../navigation';
  import StatusBadge from './StatusBadge.svelte';
  import { createRunList } from './run-list';
  import { canCancel, isRunStatus, statuses, type Run } from './run-model';

  let { query }: RouteProps = $props();
  const list = createRunList(api, events);
  const connection = events.status;
  onDestroy(list.stop);
  const date = new Intl.DateTimeFormat('en-GB', {
    dateStyle: 'medium',
    timeStyle: 'medium',
  });
  const options = [
    { value: '', label: 'All statuses' },
    ...Object.entries(statuses).map(([value, { label }]) => ({ value, label })),
  ];
  let filter = $derived(query.get('status') ?? '');
  let workflow = $derived(query.get('workflow') ?? '');
  let batch = $derived(query.get('batch') ?? '');
  let batchesView = $derived(query.get('view') === 'batches');
  const batchList = () => import('../batches/BatchList.svelte');
  let cursor = $derived(query.get('cursor') ?? undefined);
  let statusInput = $derived(isRunStatus(filter) ? filter : '');
  let workflowInput = $derived(workflow);
  let batchInput = $derived(batch);
  $effect(() => {
    if (batchesView) return;
    void list.refresh({
      status: isRunStatus(filter) ? filter : undefined,
      workflow: workflow || undefined,
      batch: batch || undefined,
      cursor,
    });
  });

  function jobsHref(
    status: string,
    workflowId: string,
    page?: string,
    batchId = batch,
  ) {
    const params = new SvelteURLSearchParams();
    if (status) params.set('status', status);
    if (workflowId) params.set('workflow', workflowId);
    if (batchId) params.set('batch', batchId);
    if (page) params.set('cursor', page);
    return `#/jobs${params.size ? `?${params}` : ''}`;
  }
  const runHref = (id: string) => `#/runs/${encodeURIComponent(id)}`;
  const time = (timestamp: string) =>
    Number.isFinite(Date.parse(timestamp))
      ? date.format(new Date(timestamp))
      : 'Not available';
  const revision = (run: Run) => `${run.prompt_id}/${run.updated_at}`;

  type Confirmation = { run: Run; rerun: boolean };
  const grants = new SvelteMap<string, string>();
  let reconcileEpoch = 0;
  onDestroy(
    events.resync.subscribe((signal) => {
      if (!signal || signal.epoch === reconcileEpoch) return;
      reconcileEpoch = signal.epoch;
      grants.clear();
    }),
  );
  const attempted = new SvelteSet<string>();
  const busy = new SvelteSet<string>();
  let confirmation = $state.raw<Confirmation>();
  let open = $state(false);
  let heading: HTMLElement;
  let notice = $state<{
    message: string;
    newId?: string;
    problem?: Problem;
  }>();

  function problem(error: unknown, title: string): Problem {
    return error instanceof ApiProblemError
      ? error.problem
      : {
          title,
          detail: error instanceof Error ? error.message : undefined,
          recovery: 'Check the latest run status before taking further action.',
        };
  }

  function confirm(run: Run, rerun = false) {
    confirmation = { run, rerun };
    open = true;
  }

  async function reconcile(run: Run) {
    if (busy.has(run.id)) return;
    busy.add(run.id);
    grants.delete(run.id);
    const epoch = reconcileEpoch;
    try {
      const result = await api.reconcileRun(run.id);
      switch (result.outcome) {
        case 'active':
          notice = {
            message:
              'ComfyUI found the prompt. GenAImage adopted it under the original run.',
          };
          break;
        case 'terminal':
          notice = {
            message:
              'The original run has reached a final status. Review its result below.',
          };
          break;
        case 'in_progress_elsewhere':
          notice = {
            message:
              'The original submission is still in progress elsewhere. Wait for it to finish before reconciling again.',
          };
          break;
        case 'not_found':
          if (result.status !== 'unknown_submission') {
            notice = {
              message:
                'The reconciliation result does not match the run status. Nothing was submitted.',
            };
            break;
          }
          if (epoch === reconcileEpoch) grants.set(run.id, revision(result));
          notice = {
            message: 'Reconciliation did not find the prompt in ComfyUI.',
          };
          break;
        default:
          notice = {
            message: result.settles_at
              ? `The submission is still ambiguous. Reconcile again at or after ${time(result.settles_at)}.`
              : 'The submission is still ambiguous. Reconciliation could not establish whether ComfyUI accepted it.',
          };
      }
      list.update(result);
    } catch (error) {
      notice = {
        message: 'Reconciliation could not be confirmed.',
        problem: problem(error, 'Could not reconcile the run'),
      };
    } finally {
      busy.delete(run.id);
    }
  }

  function rerunWasRefused(error: unknown) {
    // Only documented, definitive server rejections release the attempt. In
    // particular a timeout or an unusable response may hide a created run.
    if (!(error instanceof ApiProblemError)) return false;
    if (error.type === 'urn:genaimage:problem:rerun-mismatch') {
      return error.status === 409;
    }
    return (
      error.type === 'about:blank' &&
      [400, 403, 404, 409, 413, 421, 422].includes(error.status)
    );
  }

  async function perform() {
    if (!confirmation) return;
    const { run, rerun } = confirmation;
    const latest = $list.rows.find((row) => row.run.id === run.id)?.run;
    if (busy.has(run.id)) return;
    open = false;
    if (
      !latest ||
      (rerun
        ? latest.status !== 'unknown_submission' ||
          grants.get(run.id) !== revision(latest) ||
          revision(latest) !== revision(run) ||
          attempted.has(run.id)
        : !canCancel(latest.status))
    ) {
      notice = {
        message:
          'The run changed. Review its latest status before taking further action.',
      };
      return;
    }
    busy.add(run.id);
    try {
      if (rerun) {
        const current = await api.getRun(run.id);
        if (
          current.status !== 'unknown_submission' ||
          revision(current) !== revision(run)
        ) {
          grants.delete(run.id);
          list.update(current);
          notice = {
            message: 'The original run changed. Nothing was submitted.',
          };
          return;
        }
        if (grants.get(run.id) !== revision(run)) {
          notice = {
            message:
              'Live updates were reset. Reconcile the original run again before submitting.',
          };
          return;
        }
        const key = crypto.randomUUID();
        attempted.add(run.id);
        const result = await api.rerunRun(run.id, key);
        notice = {
          message: 'New run recorded. The original run was not changed.',
          newId: result.id,
        };
        list.update(result);
      } else {
        const result = await api.cancelRun(run.id);
        notice = {
          message:
            result.status === 'cancelled'
              ? 'Run cancelled.'
              : result.status === 'cancelling'
                ? 'Cancellation requested. GenAImage is waiting for confirmation.'
                : 'Cancellation is not confirmed. Review the latest run status.',
        };
        list.update(result);
      }
    } catch (error) {
      const refused = rerun && attempted.has(run.id) && rerunWasRefused(error);
      if (refused) {
        attempted.delete(run.id);
        grants.delete(run.id);
      }
      notice = {
        message: refused
          ? 'No new run was created. Reconcile the original run before trying again.'
          : rerun
            ? 'A new run was not confirmed. Check Jobs before taking further action.'
            : 'Cancellation could not be confirmed.',
        problem: problem(
          error,
          rerun ? 'Could not submit a new run' : 'Could not cancel the run',
        ),
      };
      void list.refresh();
    } finally {
      busy.delete(run.id);
    }
  }
</script>

<h1 tabindex="-1" bind:this={heading}>Jobs</h1>
<p>Monitor runs, request cancellation and check uncertain submissions.</p>
<nav class="views" aria-label="Jobs views">
  <a href="#/jobs" aria-current={batchesView ? undefined : 'page'}>Runs</a>
  <a href="#/jobs?view=batches" aria-current={batchesView ? 'page' : undefined}
    >Batches</a
  >
</nav>
{#if batchesView}
  {#await batchList()}
    <p role="status">Loading batches…</p>
  {:then { default: BatchList }}
    <BatchList {query} />
  {:catch}
    <ProblemDetails
      problem={{
        title: 'Could not load batches',
        recovery: 'Reload the page to try again.',
      }}
    />
  {/await}
{:else}
  <form
    class="filters"
    onsubmit={(event) => {
      event.preventDefault();
      navigateHash(
        jobsHref(
          statusInput,
          workflowInput.trim(),
          undefined,
          batchInput.trim(),
        ),
      );
    }}
  >
    <Select label="Status" bind:value={statusInput} {options} />
    <Input label="Workflow ID" bind:value={workflowInput} />
    <Input label="Batch ID" bind:value={batchInput} />
    <Button type="submit">Apply filters</Button>
    {#if filter || workflow || batch}<a href="#/jobs">Clear filters</a>{/if}
  </form>

  <p class="muted" role="status">
    {#if $connection === 'open'}Live updates connected.
    {:else}Live updates are not connected.
    {/if}
    {#if $list.loading}
      Loading runs…{/if}
  </p>
  {#if $list.error}
    <ProblemDetails problem={problem($list.error, 'Could not load runs')} />
    <Button onclick={() => list.refresh()}>Reload runs</Button>
  {/if}
  {#if notice}
    <div class="notice">
      <p role="status">
        {notice.message}
        {#if notice.newId}<a href={runHref(notice.newId)}>View new run</a>{/if}
      </p>
      {#if notice.problem}<ProblemDetails problem={notice.problem} />{/if}
    </div>
  {/if}

  {#if $list.rows.length}
    <ul class="runs" aria-label="Runs">
      {#each $list.rows as { run, node, progress } (run.id)}
        <li>
          <div class="row-heading">
            <a href={runHref(run.id)}>{run.id}</a>
            <StatusBadge status={run.status} />
            {#if run.batch_id}
              <a href={`#/batches/${encodeURIComponent(run.batch_id)}`}
                >Batch run {(run.batch_index ?? 0) + 1}</a
              >
            {/if}
          </div>
          <dl>
            <div>
              <dt>Workflow</dt>
              <dd>{run.workflow_id}</dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>
                <time datetime={run.created_at}>{time(run.created_at)}</time>
              </dd>
            </div>
            <div>
              <dt>Updated</dt>
              <dd>
                <time datetime={run.updated_at}>{time(run.updated_at)}</time>
              </dd>
            </div>
            {#if run.status === 'running'}
              <div>
                <dt>Executing node</dt>
                <dd>{node ?? 'Not reported'}</dd>
              </div>
              <div>
                <dt>Progress</dt>
                <dd>
                  {#if progress}
                    <progress
                      aria-label="Node progress"
                      value={progress.value}
                      max={progress.max}
                    ></progress>
                    {progress.value} of {progress.max}
                  {:else}Not reported{/if}
                </dd>
              </div>
            {/if}
          </dl>
          {#if canCancel(run.status)}
            <Button onclick={() => confirm(run)} disabled={busy.has(run.id)}
              >Cancel run</Button
            >
          {:else if run.status === 'unknown_submission'}
            <p>
              ComfyUI may or may not have accepted this submission. GenAImage
              will not resubmit it blindly.
            </p>
            <Button onclick={() => reconcile(run)} disabled={busy.has(run.id)}
              >Reconcile</Button
            >
            {#if grants.get(run.id) === revision(run) && !attempted.has(run.id)}
              <p>
                ComfyUI could still accept the original prompt very late.
                Submitting again could duplicate it.
              </p>
              <Button
                onclick={() => confirm(run, true)}
                disabled={busy.has(run.id)}
                >{run.batch_id
                  ? 'Submit as new run outside this batch'
                  : 'Submit as new run'}</Button
              >
              {#if run.batch_id}
                <p>The new run will not appear in this batch.</p>
              {/if}
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
  {:else if !$list.loading && !$list.error}
    <p>No runs match these filters on this page.</p>
  {/if}
  {#if cursor || $list.next}
    <nav class="row-heading" aria-label="Run pages">
      {#if cursor}<a href={jobsHref(filter, workflow)}>Newest runs</a>{/if}
      {#if $list.next}<a href={jobsHref(filter, workflow, $list.next)}
          >Older runs</a
        >{/if}
    </nav>
  {/if}
{/if}

<Dialog
  bind:open
  title={confirmation?.rerun ? 'Submit as new run' : 'Cancel run'}
  returnFocus={() => heading.focus()}
>
  {#if confirmation?.rerun}
    <p>
      Review these parameters before submitting a new run. The original run will
      not be changed. The prompt was absent from queue and history after the
      waiting period, but ComfyUI cannot prove absence. It could still accept
      the original prompt very late, so submitting again could duplicate it.
    </p>
    <p>
      <strong>Workflow:</strong>
      {confirmation.run.workflow_id} (version {confirmation.run
        .workflow_version})
    </p>
    <pre>{JSON.stringify(confirmation.run.parameters, null, 2)}</pre>
    <Button variant="primary" onclick={perform}>Confirm new run</Button>
  {:else if confirmation}
    <p>Request cancellation of {confirmation.run.id}?</p>
    <Button variant="primary" onclick={perform}>Confirm cancellation</Button>
  {/if}
</Dialog>

<style>
  .filters,
  .row-heading,
  dl {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-3);
  }
  .filters {
    align-items: end;
  }
  .views {
    display: flex;
    gap: var(--space-3);
    margin-block: var(--space-3);
  }
  .runs {
    list-style: none;
    padding: 0;
    display: grid;
    gap: var(--space-3);
  }
  li,
  .notice {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-4);
    overflow-wrap: anywhere;
  }
  dd {
    margin: 0;
  }
  dt,
  .muted {
    color: var(--muted);
    font-size: var(--text-small);
  }
  pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  progress {
    max-width: 100%;
  }
</style>
