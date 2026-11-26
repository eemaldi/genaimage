<script lang="ts">
  import { splitInlineCode } from '../diagnostics/model';

  let { text }: { text: string } = $props();
</script>

{#each splitInlineCode(text) as segment, index (index)}{#if segment.code}<code
      >{segment.text}</code
    >{:else}{segment.text}{/if}{/each}
