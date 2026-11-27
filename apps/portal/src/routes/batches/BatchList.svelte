<script lang="ts">
  import { onDestroy } from 'svelte';
  import { SvelteURLSearchParams } from 'svelte/reactivity';
  import { api, ApiProblemError, type BatchQuery } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import { stateLabels } from './labels';

  type Summary = components['schemas']['BatchSummaryResponse'];
  const pageSize = 25;
  let { query }: { query: URLSearchParams } = $props();
  let workflow = $derived(query.get('workflow') ?? '');
  let cursor = $derived(query.get('cursor') ?? undefined);
  let batches = $state.raw<Summary[]>([]);
  let next = $state<string | null>();
  let loading = $state(true);
  let error = $state.raw<unknown>();
  let controller: AbortController | undefined;
  onDestroy(() => controller?.abort());
  const date = new Intl.DateTimeFormat('en-GB', {
    dateStyle: 'medium',
    timeStyle: 'medium',
  });

  async function load(filters: BatchQuery) {
    controller?.abort();
    const request = new AbortController();
    controller = request;
    loading = true;
    error = undefined;
    try {
      const page = await api.listBatches(filters, { signal: request.signal });
      if (request.signal.aborted) return;
      batches = page.batches;
      next = page.next_cursor;
    } catch (failure) {
      if (!request.signal.aborted) error = failure;
    } finally {
      if (controller === request) loading = false;
    }
  }
  $effect(() => {
    void load({ workflow: workflow || undefined, cursor, limit: pageSize });
  });

  function href(page?: string) {
    const params = new SvelteURLSearchParams({ view: 'batches' });
    if (workflow) params.set('workflow', workflow);
    if (page) params.set('cursor', page);
    return `#/jobs?${params}`;
  }
</script>

<h2>Batches</h2>
<p>
  Each batch shows how far its submission has got. The state describes sending
  runs, not whether the images were generated.
</p>
<p role="status">{loading ? 'Loading batches…' : ''}</p>
{#if error}
  <ProblemDetails
    problem={error instanceof ApiProblemError
      ? error.problem
      : {
          title: 'Could not load batches',
          detail: error instanceof Error ? error.message : undefined,
        }}
  />
  <Button
    onclick={() =>
      load({ workflow: workflow || undefined, cursor, limit: pageSize })}
    >Reload batches</Button
  >
{/if}
{#if batches.length}
  <ul class="batches" aria-label="Batches">
    {#each batches as batch (batch.id)}
      <li>
        <a href={`#/batches/${encodeURIComponent(batch.id)}`}>{batch.id}</a>
        <strong>{stateLabels[batch.state].label}</strong>
        <span>{batch.workflow}</span>
        <time datetime={batch.created_at}
          >{Number.isFinite(Date.parse(batch.created_at))
            ? date.format(new Date(batch.created_at))
            : batch.created_at}</time
        >
        <span
          >{batch.item_count} runs · sent {batch.counts.submitted} · not sent {batch
            .counts.not_submitted + batch.counts.failed} · unknown {batch.counts
            .outcome_unknown}</span
        >
        <a href={`#/jobs?batch=${encodeURIComponent(batch.id)}`}>Runs</a>
      </li>
    {/each}
  </ul>
{:else if !loading && !error}
  <p>No batches on this page.</p>
{/if}
{#if cursor || next}
  <nav class="pager" aria-label="Batch pages">
    {#if cursor}<a href={href()}>Newest batches</a>{/if}
    {#if next}<a href={href(next)}>Older batches</a>{/if}
  </nav>
{/if}

<style>
  .batches {
    list-style: none;
    padding: 0;
    display: grid;
    gap: var(--space-3);
  }
  li {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    align-items: baseline;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-4);
    overflow-wrap: anywhere;
  }
  .pager {
    display: flex;
    gap: var(--space-3);
  }
</style>
