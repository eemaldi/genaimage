<script lang="ts">
  import type { RouteProps } from '../../router';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  let { params, query }: RouteProps = $props();
  const models = import('./Models.svelte');
</script>

{#await models}
  <h1>Models</h1>
  <p role="status">Loading models…</p>
{:then { default: Models }}
  <Models {params} {query} />
{:catch}
  <h1>Models</h1>
  <ProblemDetails
    problem={{
      title: 'Could not load the models page',
      recovery: 'Reload the page to try again.',
    }}
  />
{/await}
