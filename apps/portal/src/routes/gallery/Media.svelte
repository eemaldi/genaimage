<script lang="ts">
  import { outputMediaUrl } from '../../lib/api/client';
  import type { Output } from './gallery-model';
  let { output }: { output: Output } = $props();
  let failed = $state(false);
  let source = $derived(outputMediaUrl(output.id));
</script>

{#if failed}
  <p role="status">
    This media could not be displayed. Download the original to open it
    separately.
  </p>
{:else if output.content_type.startsWith('image/')}
  <img
    src={source}
    alt={output.source_filename}
    decoding="async"
    onerror={() => (failed = true)}
  />
{:else if output.content_type.startsWith('video/')}
  <!-- svelte-ignore a11y_media_has_caption (the generated output has no recorded caption track) -->
  <video
    src={source}
    controls
    preload="metadata"
    aria-label={output.source_filename}
    onerror={() => (failed = true)}
  ></video>
{:else if output.content_type.startsWith('audio/')}
  <audio
    src={source}
    controls
    preload="metadata"
    aria-label={output.source_filename}
    onerror={() => (failed = true)}
  ></audio>
{:else}
  <p>No preview is available for this file type.</p>
{/if}

<style>
  img,
  video {
    display: block;
    max-width: 100%;
    max-height: 55dvh;
    margin: auto;
    object-fit: contain;
  }
  audio {
    width: 100%;
  }
</style>
