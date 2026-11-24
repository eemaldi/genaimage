<script lang="ts">
  import type { ToolCall } from './model';
  let {
    toolCall,
    conversationId,
  }: { toolCall: ToolCall; conversationId: string } = $props();
</script>

<aside class="tool-placeholder" data-conversation-id={conversationId}>
  A tool call happened: <strong>{toolCall.name}</strong>.
</aside>
