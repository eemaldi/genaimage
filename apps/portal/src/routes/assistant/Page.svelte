<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import type { RouteProps } from '../../router';

  let { params: _params, query: _query }: RouteProps = $props();
  let health = $state<components['schemas']['HealthResponse']>();
  let error = $state(false);

  onMount(() => {
    let mounted = true;
    api
      .health()
      .then((value) => {
        if (mounted) health = value;
      })
      .catch(() => {
        if (mounted) error = true;
      });
    return () => {
      mounted = false;
    };
  });
</script>

<h1>Assistant</h1>
<p>The assistant is planned and not yet available.</p>
{#if error}
  <p>GenAImage could not check whether the local LLM is configured.</p>
{:else if !health}
  <p role="status">Checking LLM availability…</p>
{:else if health.llm === 'not_configured'}
  <p>
    The assistant is disabled. {health.llm_reason ??
      'Configure llm.base_url and llm.text_model in Settings to enable it.'}
  </p>
{:else if health.llm === 'unavailable'}
  <p>The assistant is disabled because the configured LLM is unavailable.</p>
  {#if health.llm_reason}<p>{health.llm_reason}</p>{/if}
{:else}
  <p>
    The LLM text role is available, but assistant conversations are not yet
    implemented.
  </p>
{/if}
