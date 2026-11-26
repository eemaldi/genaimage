<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import Choice from './Choice.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import Inline from './Inline.svelte';
  import {
    isAbort,
    latest,
    problemFrom,
    type CheckpointsResponse,
    type Choices,
    type Evidence,
  } from './model';

  let {
    unverifiable,
    choices = $bindable(),
    evidence = $bindable(),
  }: {
    unverifiable: boolean;
    choices: Choices;
    evidence: Evidence;
  } = $props();

  let result = $state<CheckpointsResponse>();
  let problem = $state<Problem>();
  let running = $state(false);
  const request = latest();

  async function load() {
    const { signal, live } = request.begin();
    running = true;
    problem = undefined;
    try {
      const next = await api.checkpoints({ signal });
      if (!live()) return;
      result = next;
      if (choices.checkpoint && !next.checkpoints.includes(choices.checkpoint))
        choices.checkpoint = undefined;
      if (choices.checkpoint)
        evidence.checkpoint = { name: choices.checkpoint };
      else delete evidence.checkpoint;
    } catch (error) {
      if (isAbort(error) || !live()) return;
      result = undefined;
      delete evidence.checkpoint;
      problem =
        error instanceof ApiProblemError && error.status === 503
          ? {
              ...error.problem,
              title: error.problem.title || 'ComfyUI is unavailable',
              recovery:
                error.problem.recovery ||
                'Start ComfyUI and pass the ComfyUI test step, then check again. This is not the same as having no checkpoints.',
            }
          : problemFrom(
              error,
              'Checkpoints could not be listed',
              'Check that the GenAImage server is running, then check again.',
            );
    } finally {
      if (live()) running = false;
    }
  }

  function choose(value: string) {
    choices.checkpoint = value || undefined;
    if (value) evidence.checkpoint = { name: value };
    else delete evidence.checkpoint;
  }

  onMount(() => {
    void load();
    return () => request.cancel();
  });
</script>

<p>
  Generating images needs at least one checkpoint (model file) in ComfyUI.
  GenAImage never installs or substitutes models.
</p>
{#if unverifiable}
  <p class="setup-problem" role="note">
    You chose a ComfyUI endpoint that is not running yet. The list below comes
    from the running server's ComfyUI, not from the endpoint you are about to
    save. Restart after saving and open the setup wizard again to verify against
    the new endpoint.
  </p>
{/if}
<p>
  <Button onclick={load} aria-busy={running}>Check again</Button>
</p>
<div role="status">
  {#if running}Listing checkpoints…{:else if result}{result.checkpoints.length}
    {result.checkpoints.length === 1 ? 'checkpoint' : 'checkpoints'} found.{/if}
</div>
{#if problem}<ProblemDetails {problem} />{/if}
{#if result}
  {#if result.checkpoints.length}
    <Choice
      label="Checkpoint for the test generation"
      hint="Nothing is chosen for you. This choice is not saved to the configuration."
      value={choices.checkpoint ?? ''}
      options={[
        { value: '', label: 'Choose a checkpoint…' },
        ...result.checkpoints.map((name) => ({ value: name, label: name })),
      ]}
      onchoose={(value) => choose(value)}
    />
  {:else}
    <div class="setup-problem" role="alert">
      <p><strong>ComfyUI reports no checkpoints.</strong></p>
      {#each result.locations as location, index (index)}
        <section>
          <h4>
            {location.environment === 'compose' ? 'Docker Compose' : 'Native'}:
            <code>{location.directory}</code>
          </h4>
          <ol>
            {#each location.steps as step, at (at)}
              <li><Inline text={step} /></li>
            {/each}
          </ol>
        </section>
      {/each}
      <p>{result.refresh_note}</p>
    </div>
  {/if}
{/if}
