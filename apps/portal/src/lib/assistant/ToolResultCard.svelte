<script lang="ts">
  let {
    tool,
    ok,
  }: {
    tool: string;
    ok: boolean;
    conversationId: string;
    toolCallId: string;
    result?: unknown;
    error?: string;
  } = $props();
</script>

<aside class="tool-placeholder">
  Tool result: {tool} ({ok ? 'succeeded' : 'failed'}).
</aside>
