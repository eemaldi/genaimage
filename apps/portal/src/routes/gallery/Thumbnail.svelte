<script lang="ts">
  import { outputThumbnailUrl } from '../../lib/api/client';
  import type { Output } from './gallery-model';

  let { output }: { output: Output } = $props();
  let failed = $state(false);
  const supported = new Set(['image/png', 'image/jpeg', 'image/webp']);
  let kind = $derived(output.content_type.split('/')[0]);
</script>

<span class="frame">
  {#if !failed && supported.has(output.content_type)}
    <img
      src={outputThumbnailUrl(output.id, 512)}
      alt=""
      width="512"
      height="384"
      loading="lazy"
      decoding="async"
      onerror={() => (failed = true)}
    />
  {:else}
    <span
      >{kind === 'image'
        ? 'Image · no thumbnail'
        : kind === 'video'
          ? 'Video'
          : kind === 'audio'
            ? 'Audio'
            : 'File'}</span
    >
  {/if}
</span>

<style>
  .frame {
    display: grid;
    place-items: center;
    width: 100%;
    height: 100%;
    min-height: 0;
    background: var(--subtle);
    color: var(--muted);
    text-align: center;
  }
  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    min-height: 0;
  }
</style>
