<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import Inline from './Inline.svelte';
  import {
    chooseEndpoint,
    isAbort,
    latest,
    problemFrom,
    type Choices,
    type DetectedService,
    type Evidence,
  } from './model';

  let {
    configured,
    editable,
    variables,
    choices = $bindable(),
    evidence = $bindable(),
  }: {
    configured: { comfyui: string; llm: string };
    editable: { comfyui: boolean; llm: boolean };
    variables: { comfyui?: string; llm?: string };
    choices: Choices;
    evidence: Evidence;
  } = $props();

  let services = $state<DetectedService[]>();
  let problem = $state<Problem>();
  let running = $state(false);
  const request = latest();
  const kinds = [
    { kind: 'comfyui', title: 'ComfyUI' },
    { kind: 'llm', title: 'LLM endpoint' },
  ] as const;

  async function detect() {
    const { signal, live } = request.begin();
    running = true;
    problem = undefined;
    try {
      const response = await api.setupDetect({ signal });
      if (!live()) return;
      services = response.services;
      evidence.detected = true;
    } catch (error) {
      if (isAbort(error) || !live()) return;
      services = undefined;
      delete evidence.detected;
      problem = problemFrom(
        error,
        'Service detection failed',
        'Check that the GenAImage server is running, then detect again.',
      );
    } finally {
      if (live()) running = false;
    }
  }

  function choose(kind: 'comfyui' | 'llm', url: string) {
    chooseEndpoint(choices, evidence, kind, url === '' ? undefined : url);
  }
  const selected = (kind: 'comfyui' | 'llm') =>
    (kind === 'comfyui' ? choices.comfyuiUrl : choices.llmUrl) ?? '';

  function candidates(kind: 'comfyui' | 'llm', items: DetectedService[]) {
    const found = items
      .filter(
        (service) =>
          service.status === 'found' && service.url !== configured[kind],
      )
      .map((service) => ({ url: service.url, stale: false }));
    const chosen = selected(kind);
    return chosen && !found.some((item) => item.url === chosen)
      ? [...found, { url: chosen, stale: true }]
      : found;
  }

  onMount(() => {
    void detect();
    return () => request.cancel();
  });
</script>

<p>
  Looks for ComfyUI and an OpenAI-compatible LLM service on this machine and at
  the allow-listed Compose hosts. Detection takes a couple of seconds and
  changes nothing.
</p>
<p>
  <Button onclick={detect} aria-busy={running}
    >{services ? 'Detect again' : 'Detect services'}</Button
  >
</p>
<div role="status">
  {running
    ? 'Detecting services…'
    : services
      ? `Detection finished: ${services.filter((s) => s.status === 'found').length} found.`
      : ''}
</div>
{#if problem}<ProblemDetails {problem} />{/if}
{#if services}
  {#each kinds as { kind, title } (kind)}
    {@const items = services.filter((service) => service.kind === kind)}
    <fieldset class="setup-group" data-kind={kind}>
      <legend>{title}</legend>
      {#if !editable[kind]}
        <p>
          The endpoint <code>{configured[kind]}</code> is set by the environment{#if variables[kind]}&#32;variable
            <code>{variables[kind]}</code>{/if} and cannot be changed here.
        </p>
      {/if}
      <ul class="setup-list">
        {#each items as service (service.url)}
          <li data-status={service.status}>
            <p>
              <strong>{service.label}</strong>
              <code>{service.url}</code>
              <span class="setup-badge">{service.status.replace('_', ' ')}</span
              >
              {#if service.configured}<span class="setup-badge"
                  >currently configured</span
                >{/if}
            </p>
            <p>{service.detail}</p>
            {#if service.version || service.flavour}
              <p>
                {#if service.version}Version {service.version}.{/if}
                {#if service.flavour}API style: {service.flavour}.{/if}
              </p>
            {/if}
            {#if service.fix && service.status !== 'found'}
              <p class="setup-fix">
                <strong>How to fix:</strong>
                <Inline text={service.fix} />
              </p>
            {/if}
          </li>
        {:else}
          <li>No candidates were checked for this service.</li>
        {/each}
      </ul>
      {#if editable[kind]}
        <p><strong>Endpoint to use</strong></p>
        <label
          ><input
            type="radio"
            name={`use-${kind}`}
            checked={selected(kind) === ''}
            onchange={() => choose(kind, '')}
          />
          Keep the configured endpoint <code>{configured[kind]}</code></label
        >
        {#each candidates(kind, items) as candidate (candidate.url)}
          <label
            ><input
              type="radio"
              name={`use-${kind}`}
              checked={selected(kind) === candidate.url}
              onchange={() => choose(kind, candidate.url)}
            />
            Use <code>{candidate.url}</code>{#if candidate.stale}
              (chosen earlier; the latest detection did not find it){/if}</label
          >
        {/each}
      {/if}
    </fieldset>
  {/each}
{/if}
