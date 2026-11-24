<script lang="ts">
  import type { RouteProps } from '../../router';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  let { params }: RouteProps = $props();
  const detail = import('./RunDetail.svelte');
</script>

<p>Run: {params.id}</p>

{#await detail}
  <h1>Run detail</h1>
  <p role="status">Loading run…</p>
{:then { default: RunDetail }}
  <RunDetail id={params.id} />
{:catch}
  <h1>Run detail</h1>
  <ProblemDetails
    problem={{
      title: 'Could not load run detail',
      recovery: 'Reload the page to try again.',
    }}
  />
{/await}
