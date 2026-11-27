<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { components } from '../../lib/api/schema';
  import type { Workflow } from '../generate/model';
  import { loadPending } from '../generate/pending';
  import Submission from '../generate/Submission.svelte';
  import { isAbort, latest, type TestGenerationState } from './model';
  import TestRun from './TestRun.svelte';
  import { starterWorkflow } from './test-run';

  let {
    state: setup,
    confirmedRunId,
    finishedRun,
    refresh,
    onConfirmed,
  }: {
    state: TestGenerationState;
    confirmedRunId?: string;
    finishedRun?: { runId: string; checkpoint?: string };
    refresh: () => Promise<boolean>;
    onConfirmed: (runId: string, run: Run) => void;
  } = $props();

  type Run = components['schemas']['RunResponse'];
  type Loaded =
    | { kind: 'loading' }
    | { kind: 'ready'; workflow: Workflow }
    | { kind: 'missing' };
  let loaded = $state.raw<Loaded>({ kind: 'loading' });
  let failure = $state.raw<Problem>();
  let checking = $state(false);
  let replaced = $state.raw<Problem>();
  const request = latest();

  let endpointLive = $derived(
    setup.activeComfyuiUrl !== undefined &&
      setup.activeComfyuiUrl === setup.comfyuiUrl,
  );
  let workflow = $derived(
    loaded.kind === 'ready' ? loaded.workflow : undefined,
  );
  let checkpoint = $derived(setup.checkpoint);
  let pendingExists = $derived.by(() => {
    try {
      return !!loadPending(starterWorkflow);
    } catch {
      return true;
    }
  });

  async function load() {
    const { signal, live } = request.begin();
    failure = undefined;
    try {
      const workflow = await api.getWorkflow(starterWorkflow, {
        signal,
        exactBindings: true,
      });
      if (live()) loaded = { kind: 'ready', workflow };
    } catch (error) {
      if (isAbort(error) || !live()) return;
      if (error instanceof ApiProblemError && error.status === 404)
        loaded = { kind: 'missing' };
      else
        failure =
          error instanceof ApiProblemError
            ? error.problem
            : {
                title: 'The starter workflow could not be loaded',
                detail: error instanceof Error ? error.message : undefined,
                recovery:
                  'Check that the GenAImage server is running, then check again.',
              };
    }
  }

  async function checkAgain() {
    if (checking) return;
    checking = true;
    try {
      await Promise.all([refresh(), load()]);
    } finally {
      checking = false;
    }
  }

  function reloadWorkflow(problem: Problem) {
    replaced = problem;
    void load();
  }

  onMount(() => {
    void load();
    return () => request.cancel();
  });
</script>

<div data-slot="test-generation">
  <p>
    The test generation runs the <code>{starterWorkflow}</code> workflow once with
    the values listed below. It runs only when you click “Run test generation”.
  </p>
  <p>
    A successful technical run is not creative approval: it shows only that
    GenAImage, ComfyUI and your checkpoint can produce an image. Judge the image
    yourself.
  </p>

  <h3>Before you can run</h3>
  <ul class="setup-list">
    <li data-check="checkpoint">
      <strong>A checkpoint is chosen:</strong>
      {setup.checkpoint ? `Yes, ${setup.checkpoint}.` : 'No.'}
      {#if !setup.checkpoint}
        <span class="setup-fix"
          >How to fix: go back to the Checkpoint step and choose an installed
          checkpoint.</span
        >{/if}
    </li>
    <li data-check="endpoint">
      <strong>The running server uses this ComfyUI:</strong>
      {#if endpointLive}Yes, {setup.comfyuiUrl}.{:else if setup.activeComfyuiUrl === undefined}
        Unknown.
      {:else}
        No. The server uses {setup.activeComfyuiUrl} but you chose {setup.comfyuiUrl}.
      {/if}
      {#if !endpointLive}
        <span class="setup-fix"
          >How to fix: restart <code>genaimage serve</code> (or the Compose
          container) so it uses {setup.comfyuiUrl}, then check again.</span
        >{/if}
    </li>
    <li data-check="workflow">
      <strong>The <code>{starterWorkflow}</code> workflow exists:</strong>
      {#if loaded.kind === 'ready'}Yes.
      {:else if !failure && loaded.kind === 'loading'}Checking…
      {:else if loaded.kind === 'missing'}
        No.
        <span class="setup-fix"
          >How to fix: add a <code>{starterWorkflow}</code> workflow (<code
            >*.genaimage.json</code
          > beside its ComfyUI API-format JSON) to the workflows directory, as described
          in the setup guide, then check again.</span
        >
      {:else if failure}Could not check.{/if}
    </li>
  </ul>
  {#if failure}<ProblemDetails problem={failure} />{/if}
  <p>
    <Button disabled={checking} onclick={checkAgain}>Check again</Button>
  </p>
  <p class="visually-hidden" role="status">
    {checking ? 'Checking the preconditions…' : ''}
  </p>

  {#if confirmedRunId}
    <p role="status" data-confirmed>
      A test generation in this session finished and the server confirmed its
      outputs. <a href={`#/runs/${encodeURIComponent(confirmedRunId)}`}
        >View the run</a
      >. That only shows the setup works; it is not creative approval.
    </p>
  {/if}

  {#if !confirmedRunId && finishedRun && finishedRun.checkpoint !== setup.checkpoint}
    <p class="setup-problem" role="status" data-finished-mismatch>
      A test run finished with checkpoint
      <code>{finishedRun.checkpoint ?? 'unknown'}</code>, not your current
      choice
      <code>{setup.checkpoint ?? 'none'}</code>.
      <a href={`#/runs/${encodeURIComponent(finishedRun.runId)}`}
        >View the run</a
      >. The step stays not done until a run uses your current choice.
    </p>
  {/if}

  {#if checkpoint && endpointLive && workflow}
    {#if replaced}<ProblemDetails problem={replaced} />{/if}
    <Submission workflowId={starterWorkflow} {reloadWorkflow} {onConfirmed}>
      {#snippet children(controls)}
        <TestRun {workflow} {checkpoint} {controls} {reloadWorkflow} />
      {/snippet}
    </Submission>
  {:else if pendingExists}
    <p class="setup-problem" role="alert">
      An earlier test submission is still pending. Fix the checks above and it
      will appear here for a retry with the same key, or check <a href="#/jobs"
        >Jobs</a
      >. Nothing is submitted until then.
    </p>
  {/if}
</div>
