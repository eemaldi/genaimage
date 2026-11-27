<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import { splitCommand, type CheckpointsResponse } from './model';

  let data = $state<CheckpointsResponse>();
  let problem = $state<Problem>();
  let loading = $state(true);
  let copied = $state('');
  let latest = 0;
  let controller: AbortController | undefined;

  async function load() {
    controller?.abort();
    const mine = ++latest;
    const current = (controller = new AbortController());
    loading = true;
    try {
      const next = await api.checkpoints({ signal: current.signal });
      if (mine !== latest) return;
      data = next;
      problem = undefined;
    } catch (error) {
      if (mine !== latest) return;
      data = undefined;
      problem =
        error instanceof ApiProblemError
          ? error.problem
          : {
              title: 'Could not list ComfyUI checkpoints',
              detail: error instanceof Error ? error.message : undefined,
              recovery:
                'Check that ComfyUI is running, then try again. This is not the same as having no checkpoints.',
            };
    } finally {
      if (mine === latest) loading = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = `Copied: ${text}`;
    } catch {
      copied = 'Could not copy. Select the command and copy it manually.';
    }
  }

  onMount(() => {
    void load();
    return () => {
      latest++;
      controller?.abort();
    };
  });
</script>

{#snippet code(text: string)}
  <code>{text}</code>
  <Button onclick={() => copy(text)} aria-label={`Copy ${text}`}>Copy</Button>
{/snippet}

<section aria-labelledby="checkpoints-heading">
  <h2 id="checkpoints-heading">ComfyUI checkpoints</h2>
  <p>
    GenAImage never downloads checkpoints. Copy the files in yourself; ComfyUI
    reports what it can see.
  </p>
  <div role="status">
    {loading ? 'Loading checkpoints…' : ''}{copied}
  </div>
  {#if problem}
    <ProblemDetails {problem} />
    <p><Button onclick={load}>Try again</Button></p>
  {/if}
  {#if data}
    <h3>Installed checkpoints ({data.checkpoints.length})</h3>
    {#if data.checkpoints.length}
      <ul>
        {#each data.checkpoints as checkpoint, index (index)}
          <li><code>{checkpoint}</code></li>
        {/each}
      </ul>
    {:else}
      <p>ComfyUI reports no checkpoints.</p>
    {/if}
    <h3>Where to put checkpoint files</h3>
    {#each data.locations as location, index (index)}
      <div class="location">
        <h4>
          {location.environment === 'compose'
            ? 'Docker Compose'
            : 'Native ComfyUI'}
        </h4>
        <p>{@render code(location.directory)}</p>
        <ol>
          {#each location.steps as step, stepIndex (stepIndex)}
            {@const part = splitCommand(step)}
            <li>
              {part.text}
              {#if part.command}<br />{@render code(part.command)}{/if}
            </li>
          {/each}
        </ol>
      </div>
    {/each}
    <p>{data.refresh_note}</p>
  {/if}
</section>

<style>
  .location {
    border-inline-start: 4px solid var(--border);
    padding-inline-start: var(--space-3);
    margin-block: var(--space-3);
  }
  code {
    overflow-wrap: anywhere;
  }
</style>
