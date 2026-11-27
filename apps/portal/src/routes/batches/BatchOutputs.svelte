<script lang="ts">
  import { onDestroy } from 'svelte';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import { api } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import Compare from '../gallery/Compare.svelte';
  import { galleryProblem, type Output } from '../gallery/gallery-model';
  import Thumbnail from '../gallery/Thumbnail.svelte';
  import { sweepLabel } from './labels';
  import type { Batch } from './poller';

  let { batch }: { batch: Batch } = $props();
  const concurrency = 4;
  const step = 24;
  const heading = 'batch-outputs-heading';

  type Card = { key: string; index: number; label: string; output: Output };
  let loaded = $state.raw<Card[]>();
  let progress = $state({ done: 0, total: 0 });
  let problems = $state.raw<ReturnType<typeof galleryProblem>[]>([]);
  let visible = $state(step);
  let selected = new SvelteSet<string>();
  let compareOpen = $state(false);
  let controller: AbortController | undefined;
  onDestroy(() => controller?.abort());

  // Only runs the backend reports as completed can have outputs.
  let completed = $derived(
    batch.items.filter(
      (item) => item.run_id && item.run_status === 'completed',
    ),
  );

  async function load() {
    controller?.abort();
    const request = new AbortController();
    controller = request;
    const queue = [...completed];
    const found = new SvelteMap<string, Card[]>();
    const failures: ReturnType<typeof galleryProblem>[] = [];
    progress = { done: 0, total: queue.length };
    problems = [];
    selected.clear();
    visible = step;
    async function worker() {
      for (let item = queue.shift(); item; item = queue.shift()) {
        try {
          const { outputs } = await api.listOutputs(
            { run: item.run_id!, limit: 50 },
            { signal: request.signal },
          );
          found.set(
            String(item.index),
            outputs.map((output) => ({
              key: output.id,
              index: item.index,
              label: `Run ${item.index + 1} · ${sweepLabel(item.sweep_values)}`,
              output,
            })),
          );
        } catch (error) {
          if (request.signal.aborted) return;
          failures.push(
            galleryProblem(
              error,
              `Could not load outputs of run ${item.index + 1}`,
            ),
          );
        }
        progress = { ...progress, done: progress.done + 1 };
      }
    }
    await Promise.all(Array.from({ length: concurrency }, worker));
    if (request.signal.aborted) return;
    loaded = [...found.values()].flat().sort((a, b) => a.index - b.index);
    problems = failures;
  }

  let chosen = $derived(
    (loaded ?? []).filter((card) => selected.has(card.key)),
  );
  let labels = $derived(
    new Map(chosen.map((card) => [card.output.id, card.label])),
  );
  let cards = $derived((loaded ?? []).slice(0, visible));
</script>

<section aria-labelledby={heading}>
  <h2 id={heading}>Outputs</h2>
  <p>
    Outputs appear only for runs that ComfyUI has confirmed as completed ({completed.length}
    of {batch.items.length} so far).
  </p>
  <Button
    onclick={load}
    disabled={!completed.length || progress.done < progress.total}
    >{loaded ? 'Reload outputs' : 'Load outputs'}</Button
  >
  <p role="status">
    {#if progress.total && progress.done < progress.total}Loading outputs:
      {progress.done} of {progress.total} runs.
    {:else if loaded}{loaded.length}
      {loaded.length === 1 ? 'output' : 'outputs'} loaded, ordered by run.{/if}
  </p>
  {#each problems as problem, index (index)}<ProblemDetails {problem} />{/each}
  {#if loaded?.length}
    <ol class="grid" aria-label="Batch outputs">
      {#each cards as card (card.key)}
        <li>
          <a
            class="frame"
            href={`#/runs/${encodeURIComponent(card.output.run_id)}`}
            ><Thumbnail output={card.output} /></a
          >
          <p>{card.label}</p>
          <label
            ><input
              type="checkbox"
              checked={selected.has(card.key)}
              onchange={(event) =>
                event.currentTarget.checked
                  ? selected.add(card.key)
                  : selected.delete(card.key)}
            />
            Select {card.label} for comparison</label
          >
        </li>
      {/each}
    </ol>
    {#if loaded.length > visible}
      <Button onclick={() => (visible += step)}
        >Show more ({loaded.length - visible} remaining)</Button
      >
    {/if}
    <Button disabled={chosen.length < 2} onclick={() => (compareOpen = true)}
      >Compare selected ({chosen.length})</Button
    >
    <Compare
      bind:open={compareOpen}
      outputs={chosen.map((card) => card.output)}
      {labels}
      returnFocus={() => document.getElementById(heading)?.focus()}
    />
  {/if}
</section>

<style>
  .grid {
    list-style: none;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
    gap: var(--space-3);
  }
  .frame {
    display: block;
    height: 9rem;
  }
  li {
    overflow-wrap: anywhere;
  }
</style>
