<script lang="ts">
  import { formatExactJson } from '../api/client';
  import Button from '../ui/Button.svelte';
  import { asRecord, generateAction } from './card-utils';

  let {
    tool,
    ok,
    result,
    error,
  }: {
    tool: string;
    ok?: boolean;
    conversationId: string;
    toolCallId: string;
    result?: unknown;
    error?: string;
  } = $props();

  const record = $derived(asRecord(result));
  const runId = $derived(
    tool === 'submit_run' && typeof record?.run_id === 'string'
      ? record.run_id
      : undefined,
  );
  const generated = $derived(
    tool === 'propose_run' && ok === true ? generateAction(result) : undefined,
  );
  const resultJson = $derived(
    result === undefined ? '' : formatExactJson(result),
  );
  const outcome = $derived(
    ok === true ? 'Succeeded' : ok === false ? 'Failed' : 'Unknown outcome',
  );
  const notes = $derived(
    Array.isArray(record?.truncation_notes)
      ? record.truncation_notes.filter(
          (note): note is string => typeof note === 'string',
        )
      : [],
  );
</script>

<details
  class:failed={ok === false}
  class:succeeded={ok === true}
  class:unknown={ok === undefined}
  class="tool-card"
>
  <summary>
    <span class="eyebrow">Tool result</span>
    <strong>{tool}</strong>
    <span class="outcome">{outcome}</span>
  </summary>
  <div class="tool-body" aria-live="polite">
    {#if error}<p class="error-text" role="alert">{error}</p>{/if}
    {#if resultJson}<p class="label">Result (exact JSON)</p>
      <pre><code>{resultJson}</code></pre>{/if}
    {#if record?.truncated === true}
      <p role="status">This tool result was truncated.</p>
      {#if notes.length}
        <ul>
          {#each notes as note, index (index)}<li>{note}</li>{/each}
        </ul>
      {/if}
    {/if}
    {#if runId}<a
        class="ui-button primary"
        href={`#/runs/${encodeURIComponent(runId)}`}>Open run {runId}</a
      >{/if}
    {#if generated?.href}
      <a class="ui-button primary" href={generated.href}>Open in Generate</a>
      <p class="note">
        Prefilled and pinned for review. The run is not submitted.
      </p>
    {:else if generated?.error}
      <Button disabled>Open in Generate</Button>
      <p class="disabled-reason" role="status">
        Open in Generate is unavailable: {generated.error}
      </p>
    {/if}
  </div>
</details>

<style>
  .tool-card {
    border: 1px solid var(--border);
    border-inline-start-width: 0.3rem;
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .succeeded {
    border-inline-start-color: var(--accent);
  }
  .failed {
    border-inline-start-color: var(--danger);
  }
  .unknown {
    border-inline-start-color: var(--muted);
  }
  summary {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3);
    cursor: pointer;
  }
  .outcome {
    margin-inline-start: auto;
  }
  .eyebrow,
  .label,
  .note,
  .disabled-reason {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .tool-body {
    display: grid;
    justify-items: start;
    gap: var(--space-2);
    padding: 0 var(--space-3) var(--space-3);
  }
  p,
  ul {
    margin: 0;
  }
  pre {
    max-inline-size: 100%;
    margin: 0;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .error-text {
    color: var(--danger);
  }
</style>
