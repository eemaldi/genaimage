<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import {
    comfyuiFix,
    isAbort,
    latest,
    problemFrom,
    type Evidence,
  } from './model';

  let {
    url,
    draftUrl,
    etag,
    evidence = $bindable(),
  }: {
    url: string;
    draftUrl?: string;
    etag: string;
    evidence: Evidence;
  } = $props();

  let outcome = $state<'ok' | 'failed'>();
  let reason = $state<string | null>();
  let problem = $state<Problem>();
  let running = $state(false);
  const request = latest();

  async function test() {
    const { signal, live } = request.begin();
    running = true;
    outcome = undefined;
    problem = undefined;
    try {
      const result = await api.testSettings(
        draftUrl ? { 'comfyui.base_url': draftUrl } : {},
        { service: 'comfyui', ifMatch: etag, signal },
      );
      if (!live()) return;
      if (result.comfyui === 'ok') {
        outcome = 'ok';
        evidence.comfyui = { url };
      } else {
        outcome = 'failed';
        reason = result.comfyui_reason;
        delete evidence.comfyui;
      }
    } catch (error) {
      if (isAbort(error) || !live()) return;
      delete evidence.comfyui;
      problem = problemFrom(
        error,
        'The ComfyUI test could not run',
        error instanceof ApiProblemError && error.status === 412
          ? 'The settings changed since the wizard loaded them. Reload this page and try again.'
          : 'Check that the GenAImage server is running, then test again.',
      );
    } finally {
      if (live()) running = false;
    }
  }

  onMount(() => {
    void test();
    return () => request.cancel();
  });
</script>

<p>
  Tests that GenAImage can reach ComfyUI at <code>{url || 'no URL'}</code>.
  Nothing is saved by this test.
</p>
<p>
  <Button onclick={test} aria-busy={running}>Test again</Button>
</p>
<div role="status">
  {#if running}Testing ComfyUI…{:else if outcome === 'ok'}ComfyUI answered at
    {url}.{:else if outcome === 'failed'}ComfyUI test failed.{/if}
</div>
{#if problem}<ProblemDetails {problem} />{/if}
{#if outcome === 'failed'}
  <div class="setup-problem" role="alert">
    <p>
      <strong>ComfyUI is not reachable at <code>{url}</code>.</strong>
    </p>
    <p class="setup-fix">
      <strong>How to fix:</strong>
      {comfyuiFix(reason)}
    </p>
  </div>
{/if}
