<script lang="ts">
  import type { RouteProps } from '../../router';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  let { query }: RouteProps = $props();
  const generate = import('./Generate.svelte');
</script>

<h1>Generate</h1>
{#await generate}
  <p role="status">Loading the generate page…</p>
{:then { default: Generate }}
  <Generate {query} />
{:catch}
  <ProblemDetails
    problem={{
      title: 'Could not load the generate page',
      recovery: 'Reload the page to try again.',
    }}
  />
{/await}
