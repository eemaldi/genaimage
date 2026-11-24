<script lang="ts">
  import { api, outputMediaUrl } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Dialog from '../../lib/ui/Dialog.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import { galleryProblem, type Output } from './gallery-model';
  import Media from './Media.svelte';
  import {
    formatRecordedValue,
    parameterDiff,
    type RecordedRow,
  } from './parameter-diff';

  let {
    open = $bindable(false),
    outputs,
    returnFocus,
  }: {
    open?: boolean;
    outputs: readonly Output[];
    returnFocus: () => void;
  } = $props();
  type Run = components['schemas']['RunResponse'];
  type Result = { run?: Run; problem?: ReturnType<typeof galleryProblem> };
  let records = $state.raw<Map<string, Result>>();
  let rows = $derived(
    parameterDiff(outputs.map((output) => records?.get(output.run_id)?.run)),
  );
  let changed = $derived(rows.filter((row) => row.different));
  let identical = $derived(
    rows.filter((row) => !row.different && !row.inexact),
  );

  let inexact = $derived(rows.filter((row) => row.inexact));

  $effect(() => {
    if (!open) return;
    const ids = [...new Set(outputs.map((output) => output.run_id))];
    const controller = new AbortController();
    records = undefined;
    void Promise.all(
      ids.map(async (id): Promise<[string, Result]> => {
        try {
          return [
            id,
            {
              run: await api.getRecordedRun(id, { signal: controller.signal }),
            },
          ];
        } catch (error) {
          return [
            id,
            { problem: galleryProblem(error, `Could not load run ${id}`) },
          ];
        }
      }),
    ).then((entries) => {
      if (!controller.signal.aborted) records = new Map(entries);
    });
    return () => controller.abort();
  });
</script>

{#snippet table(items: RecordedRow[], title: string)}
  <table>
    <caption>{title}</caption>
    <thead
      ><tr
        ><th scope="col">Field</th>{#each outputs as output, i (output.id)}<th
            scope="col">{i + 1}. {output.source_filename}</th
          >{/each}</tr
      ></thead
    >
    <tbody>
      {#each items as row (row.key)}
        <tr class:different={row.different}>
          <th scope="row">{row.label}</th>
          {#each outputs as output, i (output.id)}
            <td
              >{records?.get(output.run_id)?.problem
                ? 'Unavailable'
                : formatRecordedValue(row.values[i])}</td
            >
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

<Dialog bind:open title="Compare outputs" {returnFocus}>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex (allow keyboard scrolling in the labelled region) -->
  <div
    class="comparison"
    tabindex="0"
    role="region"
    aria-label="Side-by-side outputs and parameters"
    style:--columns={outputs.length}
  >
    <div class="sides">
      {#each outputs as output, i (output.id)}
        <section aria-label={`Output ${i + 1}`}>
          <h3>{i + 1}. {output.source_filename}</h3>
          <Media {output} />
          <a href={outputMediaUrl(output.id)} download={output.source_filename}
            >Download original</a
          >
          <a href={`#/runs/${encodeURIComponent(output.run_id)}`}>View run</a>
        </section>
      {/each}
    </div>
    {#if records}
      {#each [...records] as [id, result] (id)}
        {#if result.problem}<ProblemDetails problem={result.problem} />{/if}
      {/each}
      {#if changed.length}{@render table(changed, 'Parameter differences')}
      {:else if identical.length && !inexact.length}<p>
          No differences in the recorded fields.
        </p>{/if}
      {#if inexact.length}{@render table(
          inexact,
          'Fields that cannot be compared exactly',
        )}{/if}
      {#if identical.length}
        <details>
          <summary>Identical recorded fields ({identical.length})</summary>
          {@render table(identical, 'Identical recorded fields')}
        </details>
      {/if}
    {:else}
      <p role="status">Loading recorded parameters…</p>
    {/if}
  </div>
</Dialog>

<style>
  .comparison {
    overflow-x: auto;
  }
  .sides {
    display: grid;
    grid-template-columns: repeat(var(--columns), minmax(10rem, 1fr));
    gap: var(--space-2);
  }
  section {
    min-width: 0;
    padding: var(--space-2);
    background: var(--subtle);
  }
  section a {
    display: block;
  }
  h3 {
    margin-top: 0;
  }
  table {
    width: 100%;
    min-width: calc(10rem * var(--columns) + 6rem);
    table-layout: fixed;
    text-align: left;
    border-collapse: collapse;
    margin-block: var(--space-3);
  }
  caption {
    text-align: left;
    font-weight: var(--weight-strong);
  }
  th,
  td {
    padding: var(--space-2);
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  .different td {
    background: var(--subtle);
    white-space: pre-wrap;
  }
</style>
