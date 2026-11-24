<script lang="ts">
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import WorkflowSuggest from '../../lib/workflow-suggest/WorkflowSuggest.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import { navigateHash } from '../../navigation';
  import Form from './Form.svelte';
  import Submission, { type SubmissionControls } from './Submission.svelte';
  import { workflowHref } from './model';

  let { query }: { query: URLSearchParams } = $props();
  let id = $derived(query.get('workflow') ?? '');
  let picked = $state('');
  let reload = $state.raw<{
    queryKey: string;
    query?: URLSearchParams;
    problem: Problem;
    attempt: number;
  }>();
  let queryKey = $derived(query.toString());
  let replacement = $derived(
    reload?.queryKey === queryKey ? reload : undefined,
  );

  function reloadWorkflow(problem: Problem, restored?: URLSearchParams) {
    reload = {
      queryKey,
      query: restored,
      problem,
      attempt: (replacement?.attempt ?? 0) + 1,
    };
  }
  let request = $derived.by(() => {
    const controller = new AbortController();
    const promise = id
      ? loadWorkflow(id, controller.signal, replacement?.attempt)
      : loadCatalogue(controller.signal);
    return { controller, promise };
  });
  $effect(() => {
    const { controller } = request;
    picked = '';
    return () => controller.abort();
  });

  async function loadWorkflow(id: string, signal: AbortSignal, attempt = 0) {
    return {
      workflow: await api.getWorkflow(id, { signal, exactBindings: true }),
      attempt,
    } as const;
  }
  async function loadCatalogue(signal: AbortSignal) {
    return { catalogue: await api.listWorkflows({ signal }) } as const;
  }
  function loadProblem(error: unknown): Problem {
    return error instanceof ApiProblemError
      ? error.problem
      : {
          title: id
            ? 'Could not load the workflow'
            : 'Could not load workflows',
          detail: error instanceof Error ? error.message : undefined,
        };
  }
</script>

<WorkflowSuggest />
{#if id}<p><a href={workflowHref('')}>Choose another workflow</a></p>{/if}
{#if replacement}<ProblemDetails problem={replacement.problem} />{/if}
{#snippet workflowContent(controls?: SubmissionControls)}
  {#await request.promise}
    <p role="status">Loading {id ? 'workflow' : 'workflows'}…</p>
  {:then result}
    {#if 'workflow' in result && controls}
      {#key `${queryKey}:${result.attempt}`}<Form
          workflow={result.workflow}
          query={replacement?.query ?? query}
          {reloadWorkflow}
          {...controls}
        />{/key}
    {:else if 'catalogue' in result}
      {#if result.catalogue.workflows.length}
        <form
          onsubmit={(event) => {
            event.preventDefault();
            if (picked) navigateHash(workflowHref(picked));
          }}
        >
          <Select
            label="Workflow"
            bind:value={picked}
            options={[
              { value: '', label: 'Choose a workflow' },
              ...result.catalogue.workflows.map(({ id, name }) => ({
                value: id,
                label: `${name} (${id})`,
              })),
            ]}
            required
          />
          <Button type="submit" variant="primary" disabled={!picked}
            >Open workflow</Button
          >
        </form>
      {:else}
        <p>No workflows are available. Check the local workflow manifests.</p>
      {/if}
      {#if result.catalogue.diagnostics.length}
        <p>
          Some workflows could not be loaded. See the <a href="#/workflows"
            >workflow catalogue</a
          > for details.
        </p>
      {/if}
    {/if}
  {:catch error}
    <ProblemDetails problem={loadProblem(error)} />
    {#if replacement}<Button
        onclick={() => reloadWorkflow(replacement!.problem, replacement!.query)}
        >Retry loading workflow</Button
      >{/if}
  {/await}
{/snippet}

{#if id}
  {#key queryKey}
    <Submission workflowId={id} {reloadWorkflow}>
      {#snippet children(controls)}
        {@render workflowContent(controls)}
      {/snippet}
    </Submission>
  {/key}
{:else}
  {@render workflowContent()}
{/if}

<style>
  form {
    display: grid;
    gap: 1rem;
    max-width: 38rem;
    justify-items: start;
  }
  form :global(.ui-field) {
    width: 100%;
  }
</style>
