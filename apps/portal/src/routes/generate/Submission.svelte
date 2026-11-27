<script module lang="ts">
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import type { PendingSubmission } from './pending';
  type Rejected = (conflict?: Problem) => void;
  export type SubmissionControls = {
    editingLocked: boolean;
    accepted: boolean;
    pending?: PendingSubmission;
    submit: (snapshot: PendingSubmission, onRejected: Rejected) => void;
    resetSubmission: () => void;
  };
</script>

<script lang="ts">
  import { onDestroy, untrack, type Snippet } from 'svelte';
  import { api, ApiHttpError, ApiProblemError } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import RunProgress from './RunProgress.svelte';
  import { clearPending, loadPending, savePending } from './pending';

  type Run = components['schemas']['RunResponse'];
  let {
    workflowId,
    reloadWorkflow,
    onConfirmed,
    children,
  }: {
    workflowId: string;
    reloadWorkflow: (problem: Problem) => void;
    onConfirmed?: (runId: string, run: Run) => void;
    children: Snippet<[SubmissionControls]>;
  } = $props();
  const uid = $props.id();
  const recovered = untrack(() => {
    try {
      return { pending: loadPending(workflowId) };
    } catch (failure) {
      return {
        problem: problem(failure, 'Could not restore a pending submission'),
      };
    }
  });
  let pending = $state.raw<PendingSubmission | undefined>(recovered.pending);
  let submissionProblem = $state<Problem | undefined>(recovered.problem);
  let submitting = $state(false);
  let accepted = $state(false);
  let run = $state.raw<Run>();
  let editingLocked = $derived(submitting || !!pending || !!recovered.problem);
  let submission: AbortController | undefined;
  let onRejected: Rejected | undefined;
  onDestroy(() => submission?.abort());

  function submit(snapshot: PendingSubmission, rejected: Rejected) {
    onRejected = rejected;
    void send(snapshot, rejected);
  }

  function resetSubmission() {
    if (editingLocked) return;
    submissionProblem = undefined;
    accepted = false;
  }

  function dismissLegacy() {
    if (!pending || pending.body.workflow_revision || submitting) return;
    try {
      clearPending(pending);
      pending = undefined;
      submissionProblem = undefined;
    } catch (failure) {
      submissionProblem = problem(
        failure,
        'Could not dismiss the pending submission',
      );
    }
  }

  function problem(failure: unknown, title: string): Problem {
    return failure instanceof ApiProblemError
      ? failure.problem
      : {
          title,
          detail: failure instanceof Error ? failure.message : undefined,
        };
  }

  function workflowChanged(failure: unknown): failure is ApiProblemError {
    return (
      failure instanceof ApiProblemError &&
      failure.status === 409 &&
      failure.type === 'urn:genaimage:problem:workflow-changed'
    );
  }

  async function send(
    snapshot: PendingSubmission | undefined,
    onRejected?: (conflict?: Problem) => void,
  ) {
    if (submitting || accepted || recovered.problem) return;
    if (!snapshot?.body.workflow_revision) return;
    submitting = true;
    submissionProblem = undefined;
    const controller = new AbortController();
    submission = controller;
    try {
      if (!pending) {
        try {
          savePending(snapshot);
        } catch (failure) {
          submissionProblem = problem(failure, 'Could not save a safe retry');
          return;
        }
      }
      pending = snapshot;
      let result: Run;
      try {
        result = await api.createRun(snapshot.body, snapshot.key, {
          signal: controller.signal,
        });
        if (
          !result ||
          typeof result.id !== 'string' ||
          !result.id ||
          typeof result.status !== 'string'
        )
          throw new Error('The server did not return a run.');
      } catch (failure) {
        if (controller.signal.aborted) return;
        const status =
          failure instanceof ApiProblemError || failure instanceof ApiHttpError
            ? failure.status
            : undefined;
        const definitive =
          status !== undefined &&
          status >= 400 &&
          status < 500 &&
          status !== 408;
        submissionProblem = problem(
          failure,
          definitive
            ? 'Could not submit the run'
            : 'The submission outcome is uncertain',
        );
        if (definitive) {
          clearPending(snapshot);
          pending = undefined;
          const conflict = workflowChanged(failure)
            ? failure.problem
            : undefined;
          if (conflict) submissionProblem = undefined;
          if (onRejected) onRejected(conflict);
          else if (conflict) reloadWorkflow(conflict);
        } else if (failure instanceof ApiProblemError) {
          const { run_id: id, instance } = failure.problem;
          const runId =
            typeof id === 'string' && id
              ? id
              : instance?.match(/^\/api\/v1\/runs\/([a-zA-Z0-9_-]+)$/)?.[1];
          if (runId) {
            pending = { ...snapshot, runId };
            try {
              savePending(pending);
            } catch {
              /* The original exact request is already stored. */
            }
          }
        }
        return;
      }
      if (controller.signal.aborted) return;
      clearPending(snapshot);
      pending = undefined;
      run = result;
      accepted = true;
    } catch (failure) {
      submissionProblem = problem(
        failure,
        'Could not update the pending submission',
      );
    } finally {
      if (submission === controller) {
        submitting = false;
        submission = undefined;
      }
    }
  }
</script>

{#if submissionProblem}<ProblemDetails problem={submissionProblem} />{/if}
{#if pending}
  <section class="review" aria-labelledby={`${uid}-pending`}>
    <h3 id={`${uid}-pending`}>Pending submission</h3>
    <p>
      Workflow: <code>{pending.body.workflow}</code> · Reviewed version: {pending.version}
    </p>
    {#if pending.body.workflow_revision}<p>
        Workflow revision: <code>{pending.body.workflow_revision}</code>
      </p>{/if}
    {#if Object.keys(pending.body.parameters ?? {}).length}
      <dl>
        {#each Object.entries(pending.body.parameters ?? {}) as [name, value] (name)}
          <dt>{name}</dt>
          <dd>{value === '' ? '(empty text)' : String(value)}</dd>
        {/each}
      </dl>
    {/if}
    {#if submitting}<p role="status">Submitting the reviewed run…</p>
    {:else}
      <p>
        The run may already exist. {#if pending.runId}<a
            href={`#/runs/${encodeURIComponent(pending.runId)}`}
            >Check the run details</a
          > or
        {/if}<a href="#/jobs">check status in Jobs</a>.
      </p>
      {#if pending.body.workflow_revision}
        <p>Retrying here sends the identical request with the same key.</p>
        <Button onclick={() => send(pending, onRejected)}
          >Retry with the same key</Button
        >
      {:else}
        <p>
          This older request has no pinned revision and cannot safely be
          retried.
        </p>
        <Button onclick={dismissLegacy}
          >I've checked Jobs — dismiss this pending submission</Button
        >
      {/if}
    {/if}
  </section>
{/if}
{@render children({
  editingLocked,
  accepted,
  pending,
  submit,
  resetSubmission,
})}
{#if run}{#key run.id}<RunProgress
      initial={run}
      onconfirmed={onConfirmed}
    />{/key}{/if}

<style>
  .review {
    max-width: 50rem;
  }
  dt {
    font-weight: 600;
  }
  dd {
    margin: 0 0 0.6rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
