<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { keepFocusAcross } from '../../lib/ui/focus';
  import { SvelteURLSearchParams } from 'svelte/reactivity';
  import { api, workflowThumbnailUrl } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import WorkflowSuggest from '../../lib/workflow-suggest/WorkflowSuggest.svelte';
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import Packs from './Packs.svelte';
  import { compareSources, sourceLabel } from './model';
  import { workflowProblem } from './errors';

  let { query }: RouteProps = $props();
  type Summary = components['schemas']['WorkflowSummary'];
  type Diagnostic = components['schemas']['WorkflowDiagnostic'];
  type Listing = components['schemas']['WorkflowListResponse'];
  type Filters = { q: string; tag: string; category: string; pack: string };
  let filters = $state<Filters>({ q: '', tag: '', category: '', pack: '' });
  let workflows = $state.raw<Summary[]>([]);
  let diagnostics = $state.raw<Diagnostic[]>([]);
  let loading = $state(true);
  let refreshing = $state(false);
  let problem = $state<Problem>();
  let refreshProblem = $state<Problem>();
  let packsGeneration = $state(0);
  let catalogueQuery = $state('');
  let brokenImages = $state.raw(new Set<string>());
  let requestSeq = 0;
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());

  $effect(() => {
    filters = {
      q: query.get('q') ?? '',
      tag: query.get('tag') ?? '',
      category: query.get('category') ?? '',
      pack: query.get('pack') ?? '',
    };
    catalogueQuery = query.toString();
  });

  function apply(listing: Listing) {
    workflows = listing.workflows ?? [];
    diagnostics = listing.diagnostics ?? [];
    problem = undefined;
    refreshProblem = undefined;
  }

  async function loadCatalogue() {
    const seq = ++requestSeq;
    try {
      const listing = await api.listWorkflows({ signal: lifetime.signal });
      if (seq === requestSeq) apply(listing);
    } catch (error) {
      if (seq === requestSeq && !lifetime.signal.aborted)
        problem = workflowProblem(error);
    } finally {
      if (seq === requestSeq) loading = false;
    }
  }

  async function refresh(button: HTMLButtonElement) {
    const seq = ++requestSeq;
    const restoreFocus = keepFocusAcross(button);
    refreshing = true;
    refreshProblem = undefined;
    try {
      const listing = await api.refreshWorkflows({ signal: lifetime.signal });
      if (seq !== requestSeq) return;
      apply(listing);
      packsGeneration += 1;
    } catch (error) {
      if (seq === requestSeq && !lifetime.signal.aborted)
        refreshProblem = workflowProblem(error);
    } finally {
      refreshing = false;
      if (seq === requestSeq) loading = false;
      await restoreFocus();
    }
  }

  onMount(() => {
    void loadCatalogue();
  });

  function updateFilter(key: keyof Filters, value: string) {
    filters[key] = value;
    const parts = window.location.hash.slice(1).split('?');
    const next = new SvelteURLSearchParams(parts.slice(1).join('?'));
    if (value) next.set(key, value);
    else next.delete(key);
    const suffix = next.toString();
    catalogueQuery = suffix;
    history.replaceState(
      history.state,
      '',
      `#/workflows${suffix ? `?${suffix}` : ''}`,
    );
  }

  const detailQuery = $derived(
    catalogueQuery ? `?${new URLSearchParams({ from: catalogueQuery })}` : '',
  );

  const tags = $derived(
    [
      ...new Set(workflows.flatMap((item) => item.tags ?? []).filter(Boolean)),
    ].sort(),
  );
  const categories = $derived(
    [
      ...new Set(
        workflows
          .map((item) => item.category)
          .filter((value): value is string => !!value),
      ),
    ].sort(),
  );
  const sources = $derived(
    [...new Set(workflows.map((item) => item.source).filter(Boolean))].sort(
      compareSources,
    ),
  );
  const indexed = $derived(
    workflows.map((item) => ({
      item,
      text: [item.id, item.name, item.description ?? '', ...(item.tags ?? [])]
        .join('\n')
        .toLocaleLowerCase(),
    })),
  );
  const filtered = $derived.by(() => {
    const needle = filters.q.trim().toLocaleLowerCase();
    return indexed.filter(
      ({ item, text }) =>
        (!needle || text.includes(needle)) &&
        (!filters.tag || item.tags?.includes(filters.tag)) &&
        (!filters.category || item.category === filters.category) &&
        (!filters.pack || item.source === filters.pack),
    );
  });
</script>

<div class="heading">
  <h1>Workflows</h1>
  <Button
    disabled={refreshing}
    onclick={(event) => refresh(event.currentTarget)}>Refresh</Button
  >
</div>
{#if refreshing}<p role="status">Refreshing workflows…</p>{/if}
{#if refreshProblem}<ProblemDetails problem={refreshProblem} />{/if}
<WorkflowSuggest />
{#if loading}
  <p role="status">Loading workflows…</p>
{:else if problem}
  <ProblemDetails {problem} />
{:else}
  {#if diagnostics.length}
    <section class="diagnostics" aria-label="Workflow discovery problems">
      <p>Some workflows could not be loaded. Check their manifests:</p>
      <ul>
        {#each diagnostics as diagnostic, index (index)}<li>
            <strong>{diagnostic.path}:</strong>
            {diagnostic.error}
          </li>{/each}
      </ul>
    </section>
  {/if}
  {#if workflows.length === 0}
    <p>
      No workflows found. Add a workflow manifest to the configured workflows
      directory.
    </p>
  {:else}
    <div class="filters">
      <Input
        label="Search workflows"
        bind:value={() => filters.q, (value) => updateFilter('q', value)}
      />
      <Select
        label="Source"
        bind:value={() => filters.pack, (value) => updateFilter('pack', value)}
        options={[
          { value: '', label: 'All sources' },
          ...sources.map((value) => ({ value, label: sourceLabel(value) })),
          ...(filters.pack && !sources.includes(filters.pack)
            ? [{ value: filters.pack, label: sourceLabel(filters.pack) }]
            : []),
        ]}
      />
      <Select
        label="Tag"
        bind:value={() => filters.tag, (value) => updateFilter('tag', value)}
        options={[
          { value: '', label: 'All tags' },
          ...tags.map((value) => ({ value, label: value })),
          ...(filters.tag && !tags.includes(filters.tag)
            ? [{ value: filters.tag, label: filters.tag }]
            : []),
        ]}
      />
      <Select
        label="Category"
        bind:value={
          () => filters.category, (value) => updateFilter('category', value)
        }
        options={[
          { value: '', label: 'All categories' },
          ...categories.map((value) => ({ value, label: value })),
          ...(filters.category && !categories.includes(filters.category)
            ? [{ value: filters.category, label: filters.category }]
            : []),
        ]}
      />
    </div>
    <p class="count" role="status">
      {filtered.length}
      {filtered.length === 1 ? 'workflow' : 'workflows'} found
    </p>
    {#if filtered.length === 0}
      <p>No workflows match these filters. Try a different search or filter.</p>
    {:else}
      <ul class="grid">
        {#each filtered as { item }, index (index)}
          {@const available = item.available === true}
          <li class="card">
            <div class="thumbnail">
              {#if !available}
                <span>Thumbnail unavailable</span>
              {:else if item.thumbnail_url && !brokenImages.has(item.id)}
                <img
                  src={workflowThumbnailUrl(item.id)}
                  alt=""
                  loading="lazy"
                  onerror={() => {
                    brokenImages = new Set([...brokenImages, item.id]);
                  }}
                />
              {:else}
                <span>No thumbnail available</span>
              {/if}
            </div>
            <div class="card-body">
              <h2>
                {#if available}
                  <a
                    href={`#/workflows/${encodeURIComponent(item.id)}${detailQuery}`}
                    >{item.name || item.id}</a
                  >
                {:else}
                  {item.name || item.id}
                {/if}
              </h2>
              <p class="badges">
                <span class="badge">{sourceLabel(item.source)}</span>
                {#if !available}<span class="badge unavailable"
                    >Unavailable</span
                  >{/if}
              </p>
              {#if !available}
                <p class="reason">
                  {item.unavailable_reason ?? 'This workflow cannot be used.'}
                </p>
                <p>ID: <code>{item.id}</code></p>
              {/if}
              {#if item.description}<p>{item.description}</p>{/if}
              {#if item.tags?.length}<ul class="tags" aria-label="Tags">
                  {#each item.tags as itemTag, tagIndex (tagIndex)}<li>
                      {itemTag}
                    </li>{/each}
                </ul>{/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
{/if}
<Packs generation={packsGeneration} />

<style>
  .heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .filters {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 12rem), 1fr));
    gap: var(--space-3);
    margin-block: var(--space-4);
  }
  .count,
  .diagnostics {
    margin-block: var(--space-3);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 15rem), 1fr));
    gap: var(--space-4);
    list-style: none;
    padding: 0;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    overflow: hidden;
    min-width: 0;
  }
  .thumbnail {
    aspect-ratio: 16 / 9;
    background: var(--subtle);
    display: grid;
    place-items: center;
    color: var(--muted);
    text-align: center;
  }
  .thumbnail img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .card-body {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3);
  }
  .card h2 {
    font-size: 1.15rem;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin: 0;
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding-inline: var(--space-2);
    font-size: var(--text-small);
  }
  .badge.unavailable {
    border-color: var(--danger);
    color: var(--danger);
    font-weight: var(--weight-strong);
  }
  .reason {
    color: var(--danger);
    margin: 0;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .tags li {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding-inline: var(--space-2);
    font-size: var(--text-small);
  }
</style>
