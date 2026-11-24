<script lang="ts">
  import { tick } from 'svelte';
  import { api, outputMediaUrl } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import Dialog from '../../lib/ui/Dialog.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import { galleryProblem, type Output } from './gallery-model';
  import Media from './Media.svelte';
  import { formatRecordedValue, parameterDiff } from './parameter-diff';

  let {
    open = $bindable(false),
    id,
    output,
    error,
    previous,
    next,
    positionUnknown,
    nextLoading,
    nextProblem,
    returnFocus,
  }: {
    open?: boolean;
    id: string;
    output?: Output;
    error?: ReturnType<typeof galleryProblem>;
    previous?: () => void;
    next?: () => void;
    positionUnknown: boolean;
    nextLoading: boolean;
    nextProblem?: ReturnType<typeof galleryProblem>;
    returnFocus: () => void;
  } = $props();

  let viewport = $state<HTMLDivElement>();
  let zoom = $state(1);
  let lastDistance = 0;
  let run = $state<components['schemas']['RunResponse']>();
  let runError = $state<ReturnType<typeof galleryProblem>>();
  let rows = $derived(parameterDiff([run]));
  let imageOutput = $derived(
    output?.content_type.startsWith('image/') ?? false,
  );
  let runId = $derived(output?.run_id);

  $effect(() => {
    if (!open || !runId) return;
    const controller = new AbortController();
    run = runError = undefined;
    void api.getRecordedRun(runId, { signal: controller.signal }).then(
      (result) => {
        if (!controller.signal.aborted) run = result;
      },
      (failure: unknown) => {
        if (!controller.signal.aborted)
          runError = galleryProblem(
            failure,
            'Could not load the recorded parameters',
          );
      },
    );
    return () => controller.abort();
  });

  $effect(() => {
    if (viewport) {
      zoom = 1;
      viewport.scrollTop = viewport.scrollLeft = 0;
    }
  });

  async function changeZoom(value: number) {
    const surface = viewport;
    if (!surface) return;
    const nextZoom = Math.max(1, Math.min(6, value));
    if (nextZoom === zoom) return;
    const x = (surface.scrollLeft + surface.clientWidth / 2) / zoom;
    const y = (surface.scrollTop + surface.clientHeight / 2) / zoom;
    zoom = nextZoom;
    await tick();
    surface.scrollLeft = x * nextZoom - surface.clientWidth / 2;
    surface.scrollTop = y * nextZoom - surface.clientHeight / 2;
  }

  $effect(() => {
    const surface = viewport;
    if (!surface) return;
    function wheel(event: WheelEvent) {
      event.preventDefault();
      if (event.deltaY)
        void changeZoom(zoom * (event.deltaY < 0 ? 1.2 : 1 / 1.2));
    }
    function pinch(event: TouchEvent) {
      const [a, b] = Array.from(event.touches);
      if (!a || !b) {
        lastDistance = 0;
        return;
      }
      event.preventDefault();
      const distance = Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
      if (distance && lastDistance)
        void changeZoom((zoom * distance) / lastDistance);
      lastDistance = distance;
    }
    function stopPinch() {
      lastDistance = 0;
    }
    surface.addEventListener('wheel', wheel, { passive: false });
    surface.addEventListener('touchmove', pinch, { passive: false });
    surface.addEventListener('touchend', stopPinch);
    surface.addEventListener('touchcancel', stopPinch);
    return () => {
      stopPinch();
      surface.removeEventListener('wheel', wheel);
      surface.removeEventListener('touchmove', pinch);
      surface.removeEventListener('touchend', stopPinch);
      surface.removeEventListener('touchcancel', stopPinch);
    };
  });

  function keyboard(event: KeyboardEvent) {
    if (
      !open ||
      event.defaultPrevented ||
      event.ctrlKey ||
      event.metaKey ||
      event.altKey ||
      !(event.target instanceof HTMLElement) ||
      !event.target.closest('dialog[open]') ||
      event.target.closest('video, audio, input, select, textarea')
    )
      return;
    if (imageOutput && ['+', '=', '-', '0'].includes(event.key)) {
      event.preventDefault();
      void changeZoom(
        event.key === '0' ? 1 : event.key === '-' ? zoom / 1.25 : zoom * 1.25,
      );
    } else if (
      viewport &&
      zoom > 1 &&
      ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)
    ) {
      event.preventDefault();
      if (event.key === 'ArrowLeft') viewport.scrollLeft -= 48;
      if (event.key === 'ArrowRight') viewport.scrollLeft += 48;
      if (event.key === 'ArrowUp') viewport.scrollTop -= 48;
      if (event.key === 'ArrowDown') viewport.scrollTop += 48;
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      const navigate = event.key === 'ArrowLeft' ? previous : next;
      if (navigate) {
        event.preventDefault();
        navigate();
      }
    }
  }
</script>

<svelte:window onkeydown={keyboard} />
<Dialog bind:open title="Output preview" {returnFocus}>
  <div class="actions">
    <Button onclick={previous} disabled={!previous}>Previous</Button>
    <Button onclick={next} disabled={!next}>Next</Button>
    {#if output}<a
        href={outputMediaUrl(output.id)}
        download={output.source_filename}>Download original</a
      >{/if}
  </div>
  {#if positionUnknown}
    <p role="status">
      Previous and next are unavailable until this output appears in the loaded
      gallery results.
    </p>
  {/if}
  {#if nextLoading}<p role="status">Loading the next output…</p>{/if}
  {#if nextProblem}<ProblemDetails problem={nextProblem} />{/if}
  {#if error}
    <ProblemDetails problem={error} />
  {:else if output}
    <h3>{output.source_filename}</h3>
    {#key id}
      {#if imageOutput}
        <div class="actions">
          <Button onclick={() => changeZoom(zoom / 1.25)} disabled={zoom <= 1}
            >Zoom out</Button
          >
          <Button onclick={() => changeZoom(1)}>Reset zoom</Button>
          <Button onclick={() => changeZoom(zoom * 1.25)} disabled={zoom >= 6}
            >Zoom in</Button
          >
          <output aria-label="Zoom level">{Math.round(zoom * 100)}%</output>
        </div>
        <p class="hint">
          Use +, − or 0 to zoom. Arrow keys pan a zoomed image.
        </p>
        <div class="viewport" bind:this={viewport}>
          <div class="stage" style:--zoom={zoom}><Media {output} /></div>
        </div>
      {:else}
        <Media {output} />
      {/if}
    {/key}
    <p><a href={`#/runs/${encodeURIComponent(output.run_id)}`}>View run</a></p>
    {#if output.tags.length}<p>Tags: {output.tags.join(', ')}</p>{/if}
    <details open>
      <summary>Recorded parameters</summary>
      {#if runError}<ProblemDetails problem={runError} />
      {:else if !run}<p role="status">Loading recorded parameters…</p>
      {:else}
        <dl>
          {#each rows as row (row.key)}<dt>{row.label}</dt>
            <dd>{formatRecordedValue(row.values[0])}</dd>{/each}
        </dl>
      {/if}
    </details>
  {:else}
    <p role="status">Loading output…</p>
  {/if}
</Dialog>

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .hint,
  output {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .viewport {
    height: min(44dvh, 30rem);
    overflow: auto;
    background: var(--subtle);
    overscroll-behavior: contain;
    touch-action: pan-x pan-y;
    scrollbar-width: none;
  }
  .stage {
    width: calc(100% * var(--zoom));
    height: calc(100% * var(--zoom));
    display: grid;
    place-items: center;
  }
  .stage :global(img) {
    width: 100%;
    height: 100%;
    max-height: none;
    min-height: 0;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 2fr);
    gap: var(--space-2);
  }
  dt {
    font-weight: var(--weight-strong);
  }
  dd {
    margin: 0;
    white-space: pre-wrap;
  }
</style>
