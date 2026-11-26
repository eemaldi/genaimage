<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import Choice from './Choice.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { Settings } from '../settings/form';
  import {
    configuredValue,
    envVariable,
    isAbort,
    latest,
    problemFrom,
    readOnly,
    type Choices,
    type Evidence,
    type LlmModelsResponse,
  } from './model';

  let {
    url,
    draftUrl,
    restartPending,
    settings,
    etag,
    choices = $bindable(),
    evidence = $bindable(),
  }: {
    url: string;
    draftUrl?: string;
    restartPending: boolean;
    settings: Settings;
    etag: string;
    choices: Choices;
    evidence: Evidence;
  } = $props();

  let models = $state<string[]>();
  let roles = $state<LlmModelsResponse['roles']>();
  let problem = $state<Problem>();
  let running = $state(false);
  let source = $state<'active' | 'candidate'>('active');
  const request = latest();
  let rolesEditable = $derived(!readOnly(settings, 'llm.text_model'));

  const options = (empty: string) => [
    { value: '', label: empty },
    ...(models ?? []).map((name) => ({ value: name, label: name })),
  ];

  function record() {
    if (choices.textModel) evidence.llm = { url, textModel: choices.textModel };
    else delete evidence.llm;
  }

  async function load() {
    const { signal, live } = request.begin();
    running = true;
    problem = undefined;
    const candidate = !!draftUrl || restartPending;
    try {
      let names: string[];
      if (candidate) {
        // The models API reads the running endpoint, so a new or unrestarted
        // endpoint is listed through the connection test instead.
        const result = await api.testSettings(
          draftUrl ? { 'llm.base_url': draftUrl } : {},
          { service: 'llm', ifMatch: etag, signal },
        );
        if (!live()) return;
        if (result.llm !== 'ok')
          throw Object.assign(new Error(result.llm_reason ?? ''), {
            unreachable: true,
          });
        names = (result.llm_models ?? []).map((model) => model.id);
        roles = undefined;
      } else {
        const result = await api.llmModels({ signal });
        if (!live()) return;
        names = result.models.map((model) => model.name);
        roles = result.roles;
      }
      source = candidate ? 'candidate' : 'active';
      models = names;
      for (const key of ['textModel', 'visionModel'] as const)
        if (choices[key] && !names.includes(choices[key]))
          choices[key] = undefined;
      if (rolesEditable) {
        const text = configuredValue(settings, 'llm.text_model');
        const vision = configuredValue(settings, 'llm.vision_model');
        if (!choices.textModel && text && names.includes(text))
          choices.textModel = text;
        if (!choices.visionModel && vision && names.includes(vision))
          choices.visionModel = vision;
      } else {
        const text = roles?.find((role) => role.role === 'text');
        if (text?.state === 'present' && text.model)
          evidence.llm = { url, textModel: text.model };
      }
      if (rolesEditable) record();
    } catch (error) {
      if (isAbort(error) || !live()) return;
      models = undefined;
      delete evidence.llm;
      const unreachable =
        error instanceof Error && 'unreachable' in error ? error : undefined;
      problem = unreachable
        ? {
            title: 'The LLM endpoint is not reachable',
            detail: `Could not list models at ${url}.`,
            recovery:
              'Start the LLM service (for Ollama run `ollama serve`) and check the URL, then check again. You can also skip this step.',
          }
        : problemFrom(
            error,
            'The installed models could not be listed',
            'Check that the LLM service is running and configured, then check again. You can also skip this step.',
          );
    } finally {
      if (live()) running = false;
    }
  }

  function choose(key: 'textModel' | 'visionModel', value: string) {
    choices[key] = value || undefined;
    record();
  }

  function skip() {
    choices.llmSkipped = true;
    choices.textModel = undefined;
    choices.visionModel = undefined;
    delete evidence.llm;
    request.cancel();
    running = false;
  }

  function unskip() {
    choices.llmSkipped = false;
    void load();
  }

  onMount(() => {
    if (!choices.llmSkipped) void load();
    return () => request.cancel();
  });
</script>

{#if choices.llmSkipped}
  <p role="status">
    LLM setup is skipped. Prompt assist and the assistant stay unavailable until
    you configure a text model.
  </p>
  <Button onclick={unskip}>Set up the LLM instead</Button>
{:else}
  <p>
    The assistant needs a text model, and optionally a vision model for output
    review. Models are never chosen for you: pick them from what is installed at
    <code>{url || 'no URL'}</code>. The LLM API key is write-only and is set on
    the <a href="#/settings">Settings page</a>.
  </p>
  <p>
    <Button onclick={load} aria-busy={running}>Check again</Button>
    <Button onclick={skip}>Skip LLM setup</Button>
  </p>
  <div role="status">
    {#if running}Listing installed models…{:else if models}{models.length}
      installed {models.length === 1 ? 'model' : 'models'} found.{/if}
  </div>
  {#if problem}<ProblemDetails {problem} />{/if}
  {#if models}
    {#if source === 'candidate'}
      <p>
        These models come from the endpoint you are about to save. The Models
        page and the assistant use the running server until you restart it.
      </p>
    {/if}
    {#if !models.length}
      <div class="setup-problem" role="alert">
        <p><strong>No models are installed at this endpoint.</strong></p>
        <p class="setup-fix">
          <strong>How to fix:</strong> pull a model on the
          <a href="#/models">Models page</a>, then choose Check again.
        </p>
      </div>
    {:else if rolesEditable}
      <Choice
        label="Text model"
        hint="Required for the assistant and prompt assist."
        value={choices.textModel ?? ''}
        options={options('Choose a text model…')}
        onchoose={(value) => choose('textModel', value)}
      />
      <Choice
        label="Vision model (optional)"
        hint="Used to review generated images. Leave unset to disable vision review."
        value={choices.visionModel ?? ''}
        options={options('No vision model')}
        onchoose={(value) => choose('visionModel', value)}
      />
    {:else}
      <p>
        The model roles are set by the environment{#if envVariable(settings, 'llm.text_model')}
          variable <code>{envVariable(settings, 'llm.text_model')}</code>{/if}
        and cannot be changed here.
      </p>
      <ul>
        {#each roles ?? [] as role (role.role)}
          <li>{role.role}: {role.model ?? 'not set'} ({role.state})</li>
        {/each}
      </ul>
    {/if}
  {/if}
{/if}
