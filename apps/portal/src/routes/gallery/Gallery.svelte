<script lang="ts">
  import { untrack } from 'svelte';
  import {
    SvelteURLSearchParams,
    SvelteMap,
    SvelteSet,
  } from 'svelte/reactivity';
  import { api } from '../../lib/api/client';
  import { events } from '../../lib/api/events';
  import Button from '../../lib/ui/Button.svelte';
  import Field from '../../lib/ui/Field.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import Compare from './Compare.svelte';
  import {
    filterHash,
    filterKeys,
    galleryProblem,
    newestFirst,
    outputFilters,
    pageSize,
    statuses,
    type Output,
  } from './gallery-model';
  import Grid from './Grid.svelte';
  import Lightbox from './Lightbox.svelte';

  let { query }: { query: URLSearchParams } = $props();
  let filters = $derived(filterHash(query));
  let requested = $derived(query.get('output') || '');
  let outputs = $state.raw<Output[]>([]);
  let selected = $state.raw(new Map<string, Output>());
  let compareOpen = $state(false);
  let cursor = $state<string | null>(null);
  let busy = $state(true);
  let problem = $state<ReturnType<typeof galleryProblem>>();
  let refresh = $state.raw<() => void>();
  let more: (() => void) | undefined;
  let heading: HTMLHeadingElement;
  let advanceAfter = $state<{ id: string; filters: string; hash: string }>();

  let lightboxOpen = $derived(!!requested);
  let index = $derived(
    requested ? outputs.findIndex((item) => item.id === requested) : -1,
  );
  let cached = $state.raw<Output>();
  let metadataProblem = $state<{
    id: string;
    problem: ReturnType<typeof galleryProblem>;
  }>();
  let preview = $derived(
    outputs[index] ?? (cached?.id === requested ? cached : undefined),
  );
  let found = $derived(index >= 0);

  function url(id?: string) {
    const params = new SvelteURLSearchParams(query);
    if (id) params.set('output', id);
    else params.delete('output');
    return `#/gallery${params.size ? `?${params}` : ''}`;
  }

  function closePreview() {
    advanceAfter = undefined;
    // Do not add every dismissal to browser history or re-open it on Back.
    history.replaceState(history.state, '', url());
    window.dispatchEvent(new HashChangeEvent('hashchange'));
  }

  function navigate(id: string) {
    advanceAfter = undefined;
    history.replaceState(history.state, '', url(id));
    window.dispatchEvent(new HashChangeEvent('hashchange'));
  }

  function goNext() {
    if (!found) return;
    const neighbour = outputs[index + 1];
    if (neighbour) navigate(neighbour.id);
    else if (cursor && !advanceAfter) {
      advanceAfter = { id: requested, filters, hash: location.hash };
      if (problem) refresh?.();
      else if (!busy) more?.();
    }
  }

  $effect(() => {
    const intent = advanceAfter;
    if (!intent) return;
    if (
      intent.id !== requested ||
      intent.filters !== filters ||
      intent.hash !== location.hash
    ) {
      advanceAfter = undefined;
      return;
    }
    const neighbour = found ? outputs[index + 1] : undefined;
    if (neighbour) untrack(() => navigate(neighbour.id));
    else if (problem || !found || !cursor) advanceAfter = undefined;
    else if (!busy) untrack(() => more?.());
  });

  function applyFilters(event: SubmitEvent) {
    event.preventDefault();
    if (!(event.currentTarget instanceof HTMLFormElement)) return;
    const data = new FormData(event.currentTarget);
    const params = new SvelteURLSearchParams();
    for (const key of filterKeys) {
      const value = String(data.get(key) ?? '').trim();
      if (value) params.set(key, value);
    }
    window.location.hash = `#/gallery${params.size ? `?${params}` : ''}`;
  }

  function toggle(output: Output) {
    const next = new SvelteMap(selected);
    if (!next.delete(output.id)) next.set(output.id, output);
    selected = next;
  }

  $effect(() => {
    const key = filters;
    outputs = [];
    selected = new Map();
    cursor = null;
    compareOpen = false;
    problem = undefined;
    busy = true;
    refresh = more = undefined;
    let criteria: ReturnType<typeof outputFilters>;
    try {
      criteria = outputFilters(key);
    } catch (error) {
      busy = false;
      problem = galleryProblem(error, 'Check your gallery filters');
      return;
    }

    const controller = new AbortController();
    const { signal } = controller;
    const pending = new SvelteSet<string>();
    let active = false;
    let snapshot = true;
    let loadMore = false;
    let failed = false;
    let seenOpen = false;
    let needsRetry = false;
    let resyncEpoch = 0;

    async function fetchPage(next?: string, run?: string) {
      return api.listOutputs(
        { ...criteria, cursor: next, run, limit: pageSize },
        { signal },
      );
    }

    async function drain() {
      if (active || failed || signal.aborted) return;
      active = true;
      try {
        while (!signal.aborted && (snapshot || loadMore || pending.size)) {
          if (snapshot) {
            snapshot = loadMore = false;
            needsRetry = true;
            busy = true;
            const tail = outputs.at(-1);
            const items: Output[] = [];
            let next: string | undefined;
            do {
              const result = await fetchPage(next);
              signal.throwIfAborted();
              items.push(...result.outputs);
              next = result.next_cursor ?? undefined;
            } while (
              next &&
              tail &&
              items.length &&
              newestFirst(items[items.length - 1], tail) < 0
            );
            if (!snapshot) {
              outputs = items;
              cursor = next ?? null;
            }
            needsRetry = false;
          } else if (loadMore) {
            loadMore = false;
            if (!cursor) continue;
            busy = true;
            const result = await fetchPage(cursor);
            signal.throwIfAborted();
            if (snapshot) continue;
            const known = new Set(outputs.map((item) => item.id));
            const additions = result.outputs.filter(
              (item) => !known.has(item.id),
            );
            if (additions.length) outputs = [...outputs, ...additions];
            cursor = result.next_cursor ?? null;
          } else {
            const run = pending.values().next().value;
            if (!run) break;
            const items: Output[] = [];
            let next: string | undefined;
            do {
              const result = await fetchPage(next, run);
              signal.throwIfAborted();
              items.push(...result.outputs);
              next = result.next_cursor ?? undefined;
            } while (next);
            pending.delete(run);
            if (snapshot) continue;
            const known = new Set(outputs.map((item) => item.id));
            const tail = outputs.at(-1);
            // Older arrivals beyond the cursor will be picked up by pagination.
            const additions = items.filter(
              (item) =>
                !known.has(item.id) &&
                (!cursor || !tail || newestFirst(item, tail) < 0),
            );
            if (additions.length)
              outputs = [...outputs, ...additions].sort(newestFirst);
          }
        }
      } catch (error) {
        if (!signal.aborted) {
          failed = true;
          problem = galleryProblem(error, 'Could not load gallery outputs');
        }
      } finally {
        active = false;
        if (!signal.aborted) busy = false;
      }
    }

    function refetch() {
      snapshot = true;
      untrack(() => void drain());
    }
    refresh = () => {
      failed = false;
      problem = undefined;
      if (needsRetry || !cursor || pending.size) refetch();
      else {
        loadMore = true;
        void drain();
      }
    };
    more = () => {
      loadMore = true;
      void drain();
    };

    const status = criteria.status ?? 'completed';
    const completed = (id: string) => {
      pending.add(id);
      void drain();
    };
    const cleanup = [
      status === 'completed'
        ? events.on('run_completed', ({ run }) => completed(run.id))
        : status === 'failed'
          ? events.on('run_failed', ({ run }) => completed(run.id))
          : events.on('run_cancelled', ({ run }) => completed(run.id)),
      events.resync.subscribe((signal) => {
        if (signal && signal.epoch !== resyncEpoch) {
          resyncEpoch = signal.epoch;
          failed = false;
          problem = undefined;
          refetch();
        }
      }),
      events.status.subscribe((state) => {
        if (state === 'open' && !seenOpen) {
          seenOpen = true;
          // Close the race with the initial snapshot if the stream was still connecting.
          refetch();
        }
      }),
    ];
    untrack(() => void drain());
    return () => {
      controller.abort();
      cleanup.forEach((release) => release());
    };
  });

  $effect(() => {
    const id = requested;
    if (!id || found) return;
    const controller = new AbortController();
    metadataProblem = undefined;
    cached = undefined;
    void api.getOutputMetadata(id, { signal: controller.signal }).then(
      (output) => {
        if (!controller.signal.aborted) cached = output;
      },
      (error: unknown) => {
        if (!controller.signal.aborted)
          metadataProblem = {
            id,
            problem: galleryProblem(error, 'Could not open this output'),
          };
      },
    );
    return () => controller.abort();
  });

  const returnFocus = () => heading.focus();
</script>

<h1 tabindex="-1" bind:this={heading}>Gallery</h1>
<p>Browse saved outputs and compare the settings recorded for each run.</p>

{#key filters}
  {@const values = new URLSearchParams(filters)}
  <form class="filters" aria-label="Gallery filters" onsubmit={applyFilters}>
    <Field label="Workflow ID"
      >{#snippet children(field)}<input
          {...field}
          name="workflow"
          class="ui-control"
          value={values.get('workflow') ?? ''}
        />{/snippet}</Field
    >
    <Field label="From date (UTC)"
      >{#snippet children(field)}<input
          {...field}
          name="from"
          class="ui-control"
          type="date"
          value={values.get('from') ?? ''}
        />{/snippet}</Field
    >
    <Field label="To date (UTC)"
      >{#snippet children(field)}<input
          {...field}
          name="to"
          class="ui-control"
          type="date"
          value={values.get('to') ?? ''}
        />{/snippet}</Field
    >
    <Field label="Tag"
      >{#snippet children(field)}<input
          {...field}
          name="tag"
          class="ui-control"
          value={values.get('tag') ?? ''}
        />{/snippet}</Field
    >
    <Field label="Run status"
      >{#snippet children(field)}<select
          {...field}
          name="status"
          class="ui-control"
          value={values.get('status') ?? 'completed'}
          >{#each statuses as status (status.value)}<option value={status.value}
              >{status.label}</option
            >{/each}</select
        >{/snippet}</Field
    >
    <Button type="submit" variant="primary">Apply filters</Button>
    <a href="#/gallery">Clear filters</a>
  </form>
{/key}

<div class="actions">
  <Button disabled={selected.size < 2} onclick={() => (compareOpen = true)}
    >Compare selected ({selected.size})</Button
  >
  <Button disabled={!selected.size} onclick={() => (selected = new Map())}
    >Clear selection</Button
  >
</div>

{#if problem}
  <ProblemDetails {problem} />
  {#if refresh}<Button onclick={() => refresh?.()}>Try again</Button>{/if}
{/if}
{#if outputs.length}
  {#key filters}<Grid
      {outputs}
      {selected}
      href={url}
      {toggle}
      more={() => more?.()}
      canLoad={!!cursor && !busy && !problem}
    />{/key}
{:else if !busy && !problem}
  <p>No saved outputs match these filters.</p>
{/if}
{#if busy}<p role="status">Loading outputs…</p>
{:else if cursor && !problem}<Button onclick={() => more?.()}
    >Load more outputs</Button
  >{/if}

<Lightbox
  bind:open={
    () => lightboxOpen,
    (open) => {
      if (!open) closePreview();
    }
  }
  id={requested}
  output={preview}
  error={!preview && metadataProblem?.id === requested
    ? metadataProblem?.problem
    : undefined}
  previous={index > 0 ? () => navigate(outputs[index - 1].id) : undefined}
  next={found && !advanceAfter && (index + 1 < outputs.length || cursor)
    ? goNext
    : undefined}
  positionUnknown={!found && !!preview}
  nextLoading={!!advanceAfter}
  nextProblem={found && index === outputs.length - 1 && cursor
    ? problem
    : undefined}
  {returnFocus}
/>
<Compare
  bind:open={compareOpen}
  outputs={[...selected.values()]}
  {returnFocus}
/>

<style>
  .filters,
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--space-3);
    margin-block: var(--space-4);
  }
  .filters :global(.ui-field) {
    flex: 1 1 8rem;
    min-width: 0;
  }
</style>
