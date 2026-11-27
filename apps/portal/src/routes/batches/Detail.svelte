<script lang="ts">
  import type { RouteProps } from '../../router';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  let { params }: RouteProps = $props();
  const view = import('./BatchView.svelte');
</script>

{#await view}
  <h1>Batch</h1>
  <p role="status">Loading batch…</p>
{:then { default: BatchView }}
  {#key params.id}<BatchView id={params.id} />{/key}
{:catch}
  <h1>Batch</h1>
  <ProblemDetails
    problem={{
      title: 'Could not load the batch',
      recovery: 'Reload the page to try again.',
    }}
  />
{/await}
