<script lang="ts">
  import type { RouteProps } from '../../router';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  let { query }: RouteProps = $props();
  const gallery = import('./Gallery.svelte');
</script>

{#await gallery}
  <h1>Gallery</h1>
  <p role="status">Loading gallery…</p>
{:then { default: Gallery }}
  <Gallery {query} />
{:catch}
  <h1>Gallery</h1>
  <ProblemDetails
    problem={{
      title: 'Could not load the gallery',
      recovery: 'Reload the page to try again.',
    }}
  />
{/await}
