<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api } from '../../lib/api/client';
  import { navigateHash } from '../../navigation';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { Settings } from '../settings/form';
  import ApplyStep from './ApplyStep.svelte';
  import CheckpointStep from './CheckpointStep.svelte';
  import ComfyStep from './ComfyStep.svelte';
  import DetectStep from './DetectStep.svelte';
  import Inline from './Inline.svelte';
  import LlmStep from './LlmStep.svelte';
  import ReviewStep from './ReviewStep.svelte';
  import TestGenerationSlot from './TestGenerationSlot.svelte';
  import {
    buildReview,
    effectiveUrl,
    envOwned,
    envVariable,
    isAbort,
    isEditable,
    latest,
    llmVerified,
    patchSignature,
    problemFrom,
    rememberDismissal,
    steps,
    type Choices,
    type Evidence,
    type SetupStatus,
    type StepId,
  } from './model';

  let status = $state<SetupStatus>();
  let settings = $state<Settings>();
  let etag = $state('');
  let problem = $state<Problem>();
  let saveBlock = $state<'unknown' | 'conflict'>();
  let loading = $state(true);
  let step = $state<StepId>('detect');
  let choices = $state<Choices>({ llmSkipped: false });
  let evidence = $state<Evidence>({});
  let heading = $state<HTMLElement>();
  let statusFresh = $state(true);
  let activeComfyui = $state<string>();
  const request = latest();
  const statusRequest = latest();

  let index = $derived(steps.findIndex((item) => item.id === step));
  let comfyEditable = $derived(isEditable(settings, 'comfyui.base_url'));
  let llmEditable = $derived(isEditable(settings, 'llm.base_url'));
  let comfyUrl = $derived(effectiveUrl('comfyui', choices, settings));
  let llmUrl = $derived(effectiveUrl('llm', choices, settings));
  let review = $derived(settings ? buildReview(choices, settings) : undefined);
  let restartKeys = $derived(
    (statusFresh ? status?.restart_required : settings?.restart_required) ?? [],
  );
  let savedCurrent = $derived(
    !!review && evidence.saved?.signature === patchSignature(review.patch),
  );
  let restartUnconfirmed = $derived(
    !statusFresh && !restartKeys.length && !!evidence.saved,
  );
  let restartPending = $derived(!!restartKeys.length || restartUnconfirmed);
  let comfyChanged = $derived(
    !!status &&
      (!!choices.comfyuiUrl || restartKeys.includes('comfyui.base_url')),
  );
  let comfyVerified = $derived(evidence.comfyui?.url === comfyUrl);
  let llmDone = $derived(llmVerified(evidence, choices, llmUrl));
  let unverified = $derived(
    [
      comfyVerified ? '' : `ComfyUI at ${comfyUrl || 'no URL'} was not tested.`,
      choices.llmSkipped || llmDone
        ? ''
        : `The LLM at ${llmUrl || 'no URL'} was not verified with a chosen text model.`,
    ].filter(Boolean),
  );

  function stepStatus(id: StepId): string {
    switch (id) {
      case 'detect':
        return evidence.detected ? 'Ran' : 'Not checked';
      case 'comfyui':
        return comfyVerified ? 'Done' : 'Not verified';
      case 'llm':
        return choices.llmSkipped ? 'Skipped' : llmDone ? 'Done' : 'Not chosen';
      case 'checkpoint':
        return !evidence.checkpoint
          ? 'Not verified'
          : comfyChanged
            ? 'Verified on the running server only'
            : 'Done';
      case 'review':
        return savedCurrent
          ? 'Saved'
          : evidence.saved
            ? 'Changed since saved'
            : 'Not saved';
      case 'apply':
        if (restartKeys.length) return 'Restart required';
        if (restartUnconfirmed) return 'Restart state unconfirmed';
        return savedCurrent ? 'Done' : 'Not started';
      case 'test':
        return 'Not available yet';
    }
  }

  async function loadServerState() {
    statusRequest.cancel();
    const { signal, live } = request.begin();
    loading = true;
    try {
      const [nextStatus, snapshot] = await Promise.all([
        api.setupStatus({ signal }),
        api.getSettingsWithRevision({ signal }),
      ]);
      if (!live()) return;
      status = nextStatus;
      settings = snapshot.settings;
      etag = snapshot.etag;
      statusFresh = true;
      if (!nextStatus.restart_required.includes('comfyui.base_url'))
        activeComfyui = nextStatus.comfyui.configured_url;
      problem = undefined;
      saveBlock = undefined;
      return true;
    } catch (error) {
      if (isAbort(error) || !live()) return;
      problem = problemFrom(
        error,
        'The setup state could not be loaded',
        'Check that the GenAImage server is running, then try again.',
      );
    } finally {
      if (live()) loading = false;
    }
  }

  async function refreshStatus(): Promise<boolean> {
    const { signal, live } = statusRequest.begin();
    try {
      const next = await api.setupStatus({ signal });
      if (!live()) return false;
      status = next;
      statusFresh = true;
      problem = undefined;
      return true;
    } catch (error) {
      if (isAbort(error) || !live()) return false;
      problem = problemFrom(
        error,
        'The setup status could not be refreshed',
        'Check that the GenAImage server is running, then check again.',
      );
      return false;
    }
  }

  async function reloadSettings(): Promise<boolean> {
    return (await loadServerState()) === true;
  }

  async function onSaved(snapshot: {
    settings: Settings;
    etag: string;
    patch: Record<string, string>;
  }) {
    settings = snapshot.settings;
    etag = snapshot.etag;
    evidence.saved = { signature: patchSignature(snapshot.patch) };
    statusFresh = false;
    await refreshStatus();
    go('apply');
  }

  async function go(next: StepId) {
    step = next;
    await tick();
    heading?.focus();
  }

  function dismiss() {
    rememberDismissal(status);
    navigateHash('#/');
  }

  onMount(() => {
    void loadServerState().then((ok) => {
      // Progress comes from the server: a saved change that still needs a
      // restart resumes at the apply step after a reload.
      if (ok && status?.restart_required.length) step = 'apply';
    });
    return () => {
      request.cancel();
      statusRequest.cancel();
    };
  });
</script>

<h1>Set up GenAImage</h1>
<p>
  Connects GenAImage to ComfyUI and, optionally, a local LLM. Every step
  explains what failed and how to fix it, and nothing is saved until you review
  it.
</p>
<p>
  <Button onclick={dismiss}>Dismiss setup</Button>
  <span class="setup-hint"
    >You can reopen it from the Setup link in the navigation.</span
  >
</p>

{#if loading && !status}<p role="status">Loading the setup state…</p>{/if}
{#if problem}
  <ProblemDetails {problem} />
  {#if !status}<p><Button onclick={loadServerState}>Try again</Button></p>{/if}
{/if}

{#if status && settings}
  {#if status.needs_setup}
    <section aria-labelledby="setup-reasons">
      <h2 id="setup-reasons">Why setup is needed</h2>
      <ul class="setup-list">
        {#each status.reasons as reason (reason.code)}
          <li data-reason={reason.code}>
            <p>
              <strong>{reason.summary}</strong>
              <span class="setup-badge"
                >{reason.blocking ? 'Blocking' : 'Advice'}</span
              >
            </p>
            {#each reason.items ?? [] as item, at (at)}<p>{item}</p>{/each}
            {#if reason.fix}<p class="setup-fix">
                <strong>How to fix:</strong>
                <Inline text={reason.fix} />
              </p>{/if}
          </li>
        {/each}
      </ul>
    </section>
  {:else}
    <p role="status">
      The server reports that setup is complete. You can still review each step.
    </p>
  {/if}
  {#if status.endpoints_env_owned}
    <p class="setup-note">
      The service endpoints are set by the environment: ComfyUI
      <code>{status.comfyui.configured_url}</code
      >{#if envVariable(settings, 'comfyui.base_url')}
        (<code>{envVariable(settings, 'comfyui.base_url')}</code>){/if}, LLM
      <code>{status.llm.configured_url || 'not set'}</code
      >{#if envVariable(settings, 'llm.base_url')}
        (<code>{envVariable(settings, 'llm.base_url')}</code>){/if}. The wizard
      does not change them; the model and checkpoint steps still apply.
    </p>
  {/if}

  <nav aria-label="Setup steps">
    <ol class="setup-steps">
      {#each steps as item, at (item.id)}
        <li aria-current={item.id === step ? 'step' : undefined}>
          <span class="setup-step-number">{at + 1}</span>
          {item.label}
          <span class="setup-badge">{stepStatus(item.id)}</span>
        </li>
      {/each}
    </ol>
  </nav>
  <div class="visually-hidden" role="status">
    Step {index + 1} of {steps.length}: {steps[index].label}
  </div>

  <section class="setup-step" aria-labelledby="setup-step-heading">
    <h2 id="setup-step-heading" tabindex="-1" bind:this={heading}>
      {index + 1}. {steps[index].label}
    </h2>
    {#if step === 'detect'}
      <DetectStep
        configured={{
          comfyui: status.comfyui.configured_url,
          llm: status.llm.configured_url,
        }}
        editable={{ comfyui: comfyEditable, llm: llmEditable }}
        variables={{
          comfyui: envVariable(settings, 'comfyui.base_url'),
          llm: envVariable(settings, 'llm.base_url'),
        }}
        bind:choices
        bind:evidence
      />
    {:else if step === 'comfyui'}
      <ComfyStep
        url={comfyUrl}
        draftUrl={choices.comfyuiUrl}
        {etag}
        bind:evidence
      />
    {:else if step === 'llm'}
      <LlmStep
        url={llmUrl}
        draftUrl={choices.llmUrl}
        restartPending={restartKeys.includes('llm.base_url')}
        {settings}
        {etag}
        bind:choices
        bind:evidence
      />
    {:else if step === 'checkpoint'}
      <CheckpointStep unverifiable={comfyChanged} bind:choices bind:evidence />
    {:else if step === 'review' && review}
      <ReviewStep
        entries={review.entries}
        patch={review.patch}
        owned={envOwned(settings, choices)}
        checkpoint={choices.checkpoint}
        {settings}
        {choices}
        {etag}
        saved={savedCurrent}
        {unverified}
        {onSaved}
        reload={reloadSettings}
        blocked={saveBlock}
        onBlock={(reason) => (saveBlock = reason)}
      />
    {:else if step === 'apply'}
      <ApplyStep
        {restartKeys}
        confirmed={statusFresh}
        saved={savedCurrent}
        refresh={refreshStatus}
      />
    {:else if step === 'test'}
      <TestGenerationSlot
        state={{
          checkpoint: choices.checkpoint,
          llmTextModel: choices.textModel,
          comfyuiUrl: comfyUrl,
          activeComfyuiUrl: activeComfyui,
          savedThisSession: savedCurrent,
          configFileExists: status.config_file_exists,
          restartPending,
        }}
      />
    {/if}
  </section>

  <p class="setup-nav">
    <Button disabled={index === 0} onclick={() => go(steps[index - 1].id)}
      >Back</Button
    >
    {#if index < steps.length - 1 && step !== 'review'}
      <Button variant="primary" onclick={() => go(steps[index + 1].id)}
        >Next: {steps[index + 1].label}</Button
      >
    {/if}
  </p>
{/if}

<style>
  :global(.setup-steps) {
    list-style: none;
    margin: var(--space-4) 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  :global(.setup-steps li) {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding: var(--space-1) var(--space-3);
  }
  :global(.setup-steps li[aria-current='step']) {
    border-width: 2px;
    font-weight: 600;
  }
  :global(.setup-badge) {
    padding: 0.125rem 0.5rem;
    background: var(--subtle);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    font-size: var(--text-small);
  }
  :global(.setup-list) {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-3);
  }
  :global(.setup-list > li) {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding: var(--space-2) var(--space-4);
    background: var(--surface);
  }
  :global(.setup-group) {
    margin: var(--space-4) 0;
    display: grid;
    gap: var(--space-2);
  }
  :global(.setup-problem) {
    border: 1px solid var(--danger);
    border-inline-start-width: 4px;
    background: var(--danger-surface);
    border-radius: var(--radius-control);
    padding: var(--space-2) var(--space-4);
  }
  :global(.setup-fix) {
    font-weight: 600;
  }
  :global(.setup-table) {
    border-collapse: collapse;
    width: 100%;
  }
  :global(.setup-table th),
  :global(.setup-table td) {
    border: 1px solid var(--border);
    padding: var(--space-2);
    text-align: start;
  }
  .setup-nav {
    display: flex;
    gap: var(--space-2);
  }
  .setup-hint {
    font-size: var(--text-small);
  }
</style>
