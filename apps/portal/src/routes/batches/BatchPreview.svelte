<script module lang="ts">
  import type { SubmitBatchRequest } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import type { Scalar } from '../generate/model';

  export type Review = {
    body: Readonly<SubmitBatchRequest>;
    preview: components['schemas']['BatchPreviewResponse'];
    rows: ReadonlyArray<{
      name: string;
      value?: Scalar;
      random: boolean;
      swept: boolean;
    }>;
    key: string;
  };
  export const pageRows = 20;
</script>

<script lang="ts">
  import Button from '../../lib/ui/Button.svelte';
  import type { Workflow } from '../generate/model';
  import { describeSweep } from './labels';
  import { showValue } from './sweep';

  let {
    review,
    workflow,
    submitting,
    onsubmit,
    heading = $bindable(),
  }: {
    review: Review;
    workflow: Workflow;
    submitting: boolean;
    onsubmit: () => void;
    heading?: HTMLElement;
  } = $props();
  const uid = $props.id();
  let page = $state(0);
  let { preview } = $derived(review);
  let pages = $derived(Math.max(1, Math.ceil(preview.items.length / pageRows)));
  let shown = $derived(
    preview.items.slice(page * pageRows, (page + 1) * pageRows),
  );
  let names = $derived([
    ...new Set(preview.items.flatMap((item) => Object.keys(item.sweep_values))),
  ]);
  const inheritedSource = (
    source: components['schemas']['WorkflowInheritedSource'],
  ) =>
    source.kind === 'connection'
      ? `connected to node ${source.node} output ${source.output}`
      : source.json;
</script>

<section class="review" aria-labelledby={`${uid}-heading`}>
  <h3 id={`${uid}-heading`} tabindex="-1" bind:this={heading}>Review batch</h3>
  <p class="count">
    This will submit <strong>{preview.item_count}</strong>
    {preview.item_count === 1 ? 'run' : 'runs'}.
  </p>
  <p>
    Workflow: <code>{review.body.workflow}</code> · Version: {workflow.version}
    · Revision: <code>{review.body.workflow_revision}</code>
  </p>
  <p>
    {describeSweep(review.body.sweep)} · Maximum runs accepted: {preview.max_items}
    (server limit {preview.max_items_cap})
  </p>
  <h4>Shared values</h4>
  <dl>
    {#each review.rows.filter((row) => !row.swept) as row (row.name)}
      <dt>
        {row.name}{row.random ? ' (randomised seed, shared by every run)' : ''}
      </dt>
      <dd>
        {#if row.value === undefined}
          Inherited from the workflow (see below)
        {:else}{showValue(row.value)}{/if}
      </dd>
    {:else}
      <dt>None</dt>
      <dd>No shared values are set.</dd>
    {/each}
  </dl>
  {#if preview.inherited_inputs?.length}
    <h4>Inherited workflow inputs (the same for every run)</h4>
    <ul>
      {#each preview.inherited_inputs as input (`${input.node}:${input.input}`)}
        <li>
          {input.parameter}: {inheritedSource(input.source)}
          <span class="source">(node {input.node}, input {input.input})</span>
        </li>
      {/each}
    </ul>
  {/if}
  <h4>Every run</h4>
  <table>
    <caption>
      Runs {page * pageRows + 1} to {page * pageRows + shown.length} of {preview.item_count}
    </caption>
    <thead>
      <tr>
        <th scope="col">Run</th>
        {#each names as name (name)}<th scope="col">{name}</th>{/each}
        <th scope="col">All values</th>
      </tr>
    </thead>
    <tbody>
      {#each shown as item (item.index)}
        <tr>
          <th scope="row">{item.index + 1}</th>
          {#each names as name (name)}
            <td>{showValue(item.sweep_values[name])}</td>
          {/each}
          <td>
            <details>
              <summary>Show every value</summary>
              <dl>
                {#each Object.entries(item.effective_parameters) as [name, value] (name)}
                  <dt>{name}</dt>
                  <dd>{showValue(value)}</dd>
                {/each}
              </dl>
            </details>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if pages > 1}
    <nav aria-label="Review pages" class="pager">
      <Button disabled={page === 0} onclick={() => page--}>Previous runs</Button
      >
      <span>Page {page + 1} of {pages}</span>
      <Button disabled={page >= pages - 1} onclick={() => page++}
        >Next runs</Button
      >
    </nav>
  {/if}
  <p>
    Submitting sends all {preview.item_count}
    {preview.item_count === 1 ? 'run' : 'runs'} as ordinary runs, one at a time. A
    successful submission does not mean the images are finished or approved.
  </p>
  <Button variant="primary" disabled={submitting} onclick={onsubmit}
    >Submit {preview.item_count}
    {preview.item_count === 1 ? 'run' : 'runs'}</Button
  >
</section>

<style>
  .review {
    max-width: 60rem;
  }
  .count {
    font-size: var(--text-large, 1.25rem);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    text-align: left;
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
    overflow-wrap: anywhere;
  }
  dt {
    font-weight: 600;
  }
  dd {
    margin: 0 0 0.6rem;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .source {
    color: var(--muted);
  }
  .pager {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
</style>
