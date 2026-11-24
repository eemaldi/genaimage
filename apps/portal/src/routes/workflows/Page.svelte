<script lang="ts">
  import { SvelteURLSearchParams } from 'svelte/reactivity';
  import { api, workflowThumbnailUrl } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import { workflowProblem } from './errors';

  let { query }: RouteProps = $props();
  type Summary = components['schemas']['WorkflowSummary'];
  type Diagnostic = components['schemas']['WorkflowDiagnostic'];
  let workflows = $state.raw<Summary[]>([]);
  let diagnostics = $state.raw<Diagnostic[]>([]);
  let loading = $state(true);
  let problem = $state<Problem>();
  let search = $state('');
  let tag = $state('');
  let category = $state('');
  let brokenImages = $state.raw(new Set<string>());

  $effect(() => {
    search = query.get('q') ?? '';
    tag = query.get('tag') ?? '';
    category = query.get('category') ?? '';
  });

  $effect(() => {
    const controller = new AbortController();
    api
      .listWorkflows({ signal: controller.signal })
      .then((result) => {
        if (controller.signal.aborted) return;
        workflows = result.workflows;
        diagnostics = result.diagnostics;
        loading = false;
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        problem = workflowProblem(error);
        loading = false;
      });
    return () => controller.abort();
  });

  function updateFilter(key: 'q' | 'tag' | 'category', value: string) {
    if (key === 'q') search = value;
    else if (key === 'tag') tag = value;
    else category = value;
    const parts = window.location.hash.slice(1).split('?');
    const next = new SvelteURLSearchParams(parts.slice(1).join('?'));
    if (value) next.set(key, value);
    else next.delete(key);
    const suffix = next.toString();
    history.replaceState(
      history.state,
      '',
      `#/workflows${suffix ? `?${suffix}` : ''}`,
    );
  }

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
  const indexed = $derived(
    workflows.map((item) => ({
      item,
      text: [item.name, item.description ?? '', ...(item.tags ?? [])]
        .join('\n')
        .toLocaleLowerCase(),
    })),
  );
  const filtered = $derived.by(() => {
    const needle = search.trim().toLocaleLowerCase();
    return indexed.filter(
      ({ item, text }) =>
        (!needle || text.includes(needle)) &&
        (!tag || item.tags?.includes(tag)) &&
        (!category || item.category === category),
    );
  });
</script>

<h1>Workflows</h1>
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
        bind:value={() => search, (value) => updateFilter('q', value)}
      />
      <Select
        label="Tag"
        bind:value={() => tag, (value) => updateFilter('tag', value)}
        options={[
          { value: '', label: 'All tags' },
          ...tags.map((value) => ({ value, label: value })),
          ...(tag && !tags.includes(tag) ? [{ value: tag, label: tag }] : []),
        ]}
      />
      <Select
        label="Category"
        bind:value={() => category, (value) => updateFilter('category', value)}
        options={[
          { value: '', label: 'All categories' },
          ...categories.map((value) => ({ value, label: value })),
          ...(category && !categories.includes(category)
            ? [{ value: category, label: category }]
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
        {#each filtered as { item } (item)}
          <li class="card">
            <div class="thumbnail">
              {#if item.thumbnail_url && !brokenImages.has(item.id)}
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
                <a href={`#/workflows/${encodeURIComponent(item.id)}`}
                  >{item.name || item.id}</a
                >
              </h2>
              {#if item.description}<p>{item.description}</p>{/if}
              {#if item.tags?.length}<ul class="tags" aria-label="Tags">
                  {#each item.tags as itemTag, index (index)}<li>
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

<style>
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
