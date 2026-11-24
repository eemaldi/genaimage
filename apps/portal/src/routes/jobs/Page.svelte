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
  let cursor = $derived(query.get('cursor') ?? undefined);
  let statusInput = $derived(isRunStatus(filter) ? filter : '');
  let workflowInput = $derived(workflow);
  $effect(() => {
    void list.refresh({
      status: isRunStatus(filter) ? filter : undefined,
      workflow: workflow || undefined,
      cursor,
    });
  });

  function jobsHref(status: string, workflowId: string, page?: string) {
    const params = new SvelteURLSearchParams();
    if (status) params.set('status', status);
    if (workflowId) params.set('workflow', workflowId);
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
  // The server-side rerun endpoint owns binding and fingerprint verification.
  const rerunClient = api as typeof api & {
    rerunRun?: (id: string, key: string) => Promise<Run>;
  };
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

  async function perform() {
    if (!confirmation) return;
    const { run, rerun } = confirmation;
    const latest = $list.rows.find((row) => row.run.id === run.id)?.run;
    if (busy.has(run.id)) return;
    open = false;
    if (
      !latest ||
      (rerun
        ? !rerunClient.rerunRun ||
          latest.status !== 'unknown_submission' ||
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
      if (rerun && rerunClient.rerunRun) {
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
        const result = await rerunClient.rerunRun(run.id, key);
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
      notice = {
        message: rerun
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
<form
  class="filters"
  onsubmit={(event) => {
    event.preventDefault();
    window.location.hash = jobsHref(statusInput, workflowInput.trim());
  }}
>
  <Select label="Status" bind:value={statusInput} {options} />
  <Input label="Workflow ID" bind:value={workflowInput} />
  <Button type="submit">Apply filters</Button>
  {#if filter || workflow}<a href="#/jobs">Clear filters</a>{/if}
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
            ComfyUI may or may not have accepted this submission. GenAImage will
            not resubmit it blindly.
          </p>
          <Button onclick={() => reconcile(run)} disabled={busy.has(run.id)}
            >Reconcile</Button
          >
          {#if grants.get(run.id) === revision(run) && !attempted.has(run.id)}
            <p>
              ComfyUI could still accept the original prompt very late.
              Submitting again could duplicate it.
            </p>
            {#if rerunClient.rerunRun}
              <Button
                onclick={() => confirm(run, true)}
                disabled={busy.has(run.id)}>Submit as new run</Button
              >
            {:else}
              <p>
                Submitting as a new run is unavailable until the server can
                verify the original workflow and inputs.
              </p>
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

<Dialog
  bind:open
  title={confirmation?.rerun ? 'Submit as new run' : 'Cancel run'}
  returnFocus={() => heading.focus()}
>
  {#if confirmation?.rerun}
    <p>
      Review these parameters before submitting a new run. The original run will
      not be changed. If ComfyUI accepted the original prompt very late, this
      could duplicate it.
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
