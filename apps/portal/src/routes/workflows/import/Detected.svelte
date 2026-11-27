<script lang="ts">
  import Button from '../../../lib/ui/Button.svelte';
  import { groupByClass, maxListed, type Proposal } from './model';

  let { proposal }: { proposal: Proposal } = $props();
  let recognisedShown = $state(maxListed);
  let unboundShown = $state(maxListed);
  let unknown = $derived(groupByClass(proposal.unknown_nodes));
  let linked = $derived(proposal.unbound_inputs.filter((item) => item.linked));
  let literal = $derived(
    proposal.unbound_inputs.filter((item) => !item.linked),
  );
  let linkedByClass = $derived(
    groupByClass(linked.map(({ node, class_type }) => ({ node, class_type }))),
  );
</script>

<section aria-labelledby="detected-heading">
  <h3 id="detected-heading">What the importer found</h3>
  {#each proposal.warnings as warning, at (at)}
    <p class="warning" role="note">{warning}</p>
  {/each}

  <details>
    <summary>Recognised nodes ({proposal.recognised.length})</summary>
    <p>These nodes have import rules. Their bound inputs became parameters.</p>
    <ul>
      {#each proposal.recognised.slice(0, recognisedShown) as item (item.node)}
        <li>
          Node {item.node}, <code>{item.class_type}</code>:
          {item.bound_inputs.length
            ? item.bound_inputs
                .map(({ input, parameter }) => `${input} as ${parameter}`)
                .join(', ')
            : 'no bound inputs'}
        </li>
      {/each}
    </ul>
    {#if proposal.recognised.length > recognisedShown}
      <p>
        {proposal.recognised.length - recognisedShown} more not shown.
        <Button onclick={() => (recognisedShown += 200)}>Show more</Button>
      </p>
    {/if}
  </details>

  <details>
    <summary
      >Nodes without import rules ({proposal.unknown_nodes.length})</summary
    >
    <p>
      These nodes keep the values in the workflow file. Nothing is guessed, and
      the list does not mean they are unsupported.
    </p>
    <ul>
      {#each unknown as group (group.class_type)}
        <li>
          <code>{group.class_type}</code>: {group.count}
          {group.count === 1 ? 'node' : 'nodes'}
        </li>
      {/each}
    </ul>
  </details>

  <details>
    <summary
      >Inputs without a parameter ({proposal.unbound_inputs.length})</summary
    >
    <p>
      Inputs of recognised nodes that no parameter controls. The workflow value
      is used.
    </p>
    {#if linked.length}
      <p>
        {linked.length} of them are connections to other nodes, which cannot be bound:
        {linkedByClass
          .map((group) => `${group.class_type} (${group.count})`)
          .join(', ')}.
      </p>
    {/if}
    {#if literal.length}
      <ul>
        {#each literal.slice(0, unboundShown) as item, at (at)}
          <li>
            Node {item.node}, <code>{item.class_type}</code>,
            <code>{item.input}</code>: {item.reason}
          </li>
        {/each}
      </ul>
      {#if literal.length > unboundShown}
        <p>
          {literal.length - unboundShown} more not shown.
          <Button onclick={() => (unboundShown += 200)}>Show more</Button>
        </p>
      {/if}
    {/if}
  </details>
</section>

<style>
  .warning {
    padding: 0.5rem;
    background: var(--subtle);
  }
  ul {
    padding-inline-start: 1.2rem;
  }
</style>
