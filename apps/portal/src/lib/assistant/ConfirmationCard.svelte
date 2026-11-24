<script lang="ts">
  import type { Confirmation } from './model';
  let { confirmation }: { confirmation: Confirmation; conversationId: string } =
    $props();
</script>

<aside class="confirmation-placeholder">
  Confirmation required for <strong>{confirmation.tool}</strong>.
</aside>
