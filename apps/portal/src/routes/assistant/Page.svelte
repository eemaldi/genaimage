<script lang="ts">
  import { onMount, type Component } from 'svelte';
  import type { RouteProps } from '../../router';

  let { params: _params, query: _query }: RouteProps = $props();
  let AssistantPanel = $state<Component>();
  let failed = $state(false);

  onMount(() => {
    void import('../../lib/assistant/AssistantPanel.svelte')
      .then((module) => (AssistantPanel = module.default))
      .catch(() => (failed = true));
  });
</script>

<h1>Assistant</h1>
{#if AssistantPanel}
  <AssistantPanel presentation="full" />
{:else if failed}
  <p>The assistant interface could not be loaded.</p>
{:else}
  <p role="status">Loading assistant…</p>
{/if}
