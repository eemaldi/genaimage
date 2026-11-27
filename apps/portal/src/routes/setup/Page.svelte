<script lang="ts">
  import type { Component } from 'svelte';
  import { onMount } from 'svelte';
  import type { RouteProps } from '../../router';

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
  <h1>Set up GenAImage</h1>
  <p role="alert">
    The setup wizard could not be loaded. Reload the page; if it keeps failing,
    rebuild the portal.
  </p>
{:else}
  <h1>Set up GenAImage</h1>
  <p role="status">Loading the setup wizard…</p>
{/if}
