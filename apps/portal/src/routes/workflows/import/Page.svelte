<script lang="ts">
  import type { Component } from 'svelte';
  import { onMount } from 'svelte';
  import type { RouteProps } from '../../../router';

  let { params: _params, query: _query }: RouteProps = $props();
  let Wizard = $state<Component>();
  let failed = $state(false);

  onMount(() => {
    import('./Wizard.svelte').then(
      (module) => (Wizard = module.default),
      () => (failed = true),
    );
  });
</script>

{#if Wizard}
  <Wizard />
{:else if failed}
  <h1>Import a workflow</h1>
  <p role="alert">
    The import wizard could not be loaded. Reload the page; if it keeps failing,
    rebuild the portal.
  </p>
{:else}
  <h1>Import a workflow</h1>
  <p role="status">Loading the import wizard…</p>
{/if}
