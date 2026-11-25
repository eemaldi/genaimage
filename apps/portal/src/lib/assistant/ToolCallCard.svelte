<script lang="ts">
  import { formatExactJson } from '../api/client';
  import type { ToolCall } from './model';

  let {
    toolCall,
    conversationId,
  }: { toolCall: ToolCall; conversationId: string } = $props();
  const formatted = $derived.by(() => {
    try {
      return { json: formatExactJson(toolCall.arguments), error: '' };
    } catch (error) {
      return {
        json: '',
        error:
          error instanceof Error
            ? error.message
            : 'The arguments cannot be shown.',
      };
    }
  });
</script>

<details class="tool-card" data-conversation-id={conversationId}>
  <summary>
    <span class="eyebrow">Tool call</span>
    <strong>{toolCall.name}</strong>
  </summary>
  <div class="tool-body">
    <p class="label">Arguments (exact JSON)</p>
    {#if formatted.error}<p class="error-text" role="alert">
        {formatted.error}
      </p>{/if}
    <pre><code>{formatted.json}</code></pre>
  </div>
</details>

<style>
  .tool-card {
    background: var(--subtle);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  summary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3);
    cursor: pointer;
  }
  .eyebrow,
  .label {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .tool-body {
    display: grid;
    gap: var(--space-2);
    padding: 0 var(--space-3) var(--space-3);
  }
  p {
    margin: 0;
  }
  pre {
    margin: 0;
    max-inline-size: 100%;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .error-text {
    color: var(--danger);
  }
</style>
