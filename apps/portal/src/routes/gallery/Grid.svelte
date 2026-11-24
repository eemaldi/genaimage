<script lang="ts">
  import { tick, untrack } from 'svelte';
  import type { Output } from './gallery-model';
  import Thumbnail from './Thumbnail.svelte';
  import { gridGeometry, virtualRows } from './virtualiser';

  let {
    outputs,
    selected,
    href,
    toggle,
    more,
    canLoad,
  }: {
    outputs: readonly Output[];
    selected: ReadonlyMap<string, Output>;
    href: (id: string) => string;
    toggle: (output: Output) => void;
    more: () => void;
    canLoad: boolean;
  } = $props();

  let viewport: HTMLDivElement;
  let width = $state(0);
  let height = $state(0);
  let top = $state(0);
  let geometry = $derived(gridGeometry(width || 720));
  let range = $derived(
    virtualRows(
      outputs.length,
      geometry.columns,
      geometry.rowHeight,
      top,
      height || 480,
    ),
  );
  let previous:
    | { outputs: readonly Output[]; columns: number; rowHeight: number }
    | undefined;

  $effect.pre(() => {
    const current = { outputs, ...geometry };
    const old = previous;
    previous = current;
    const scroll = untrack(() => top);
    if (!old || !scroll) return;
    const row = Math.floor(scroll / old.rowHeight);
    const index = row * old.columns;
    const anchor = old.outputs[index];
    if (!anchor) return;
    const nextIndex =
      current.outputs[index]?.id === anchor.id
        ? index
        : current.outputs.findIndex((item) => item.id === anchor.id);
    if (
      nextIndex < 0 ||
      (old.rowHeight === current.rowHeight &&
        old.columns === current.columns &&
        row === Math.floor(nextIndex / current.columns))
    )
      return;
    const offset = scroll % old.rowHeight;
    void tick().then(() => {
      if (!viewport?.isConnected || previous !== current) return;
      viewport.scrollTop =
        Math.floor(nextIndex / current.columns) * current.rowHeight +
        Math.min(offset, current.rowHeight - 1);
      top = viewport.scrollTop;
    });
  });

  $effect(() => {
    if (
      canLoad &&
      top + (height || 480) >= range.totalHeight - 2 * geometry.rowHeight
    )
      untrack(more);
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (allow keyboard scrolling in the labelled region) -->
<div
  class="viewport"
  role="region"
  aria-label="Gallery outputs"
  tabindex="0"
  bind:this={viewport}
  bind:clientWidth={width}
  bind:clientHeight={height}
  onscroll={() => (top = viewport.scrollTop)}
>
  <div class="extent" style:height={`${range.totalHeight}px`}>
    <ul
      aria-label="Loaded outputs"
      class="tiles"
      style:top={`${range.offset}px`}
      style:--columns={geometry.columns}
      style:--tile-height={`${geometry.rowHeight - 12}px`}
    >
      {#each outputs.slice(range.start, range.end) as output, i (output.id)}
        <li aria-posinset={range.start + i + 1} aria-setsize={outputs.length}>
          <a
            class="preview"
            href={href(output.id)}
            aria-label={`Open ${output.source_filename}`}
            ><Thumbnail {output} /></a
          >
          <span class="filename" title={output.source_filename}
            >{output.source_filename}</span
          >
          <label
            ><input
              type="checkbox"
              checked={selected.has(output.id)}
              onchange={() => toggle(output)}
              aria-label={`Select ${output.source_filename} for comparison`}
            /> Compare</label
          >
        </li>
      {/each}
    </ul>
  </div>
</div>

<style>
  .viewport {
    height: min(62dvh, 42rem);
    min-height: 12rem;
    overflow: auto;
    overflow-anchor: none;
    scrollbar-gutter: stable;
  }
  .extent {
    position: relative;
  }
  .tiles {
    position: absolute;
    inset-inline: 0;
    display: grid;
    grid-template-columns: repeat(var(--columns), minmax(0, 1fr));
    gap: 12px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    box-sizing: border-box;
    display: grid;
    grid-template-rows: minmax(0, 1fr) 2rem 2.5rem;
    min-width: 0;
    height: var(--tile-height);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--surface);
  }
  .preview {
    display: block;
    min-height: 0;
  }
  .preview:focus-visible {
    outline-offset: -3px;
  }
  .filename {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    align-self: center;
    padding-inline: var(--space-2);
    font-size: var(--text-small);
  }
  label {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding-inline: var(--space-2);
    font-size: var(--text-small);
    cursor: pointer;
  }
</style>
