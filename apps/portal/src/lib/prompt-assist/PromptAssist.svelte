<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import {
    api,
    ApiProblemError,
    type PromptAssistRequest,
  } from '../api/client';
  import type { components } from '../api/schema';
  import Button from '../ui/Button.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import Input from '../ui/Input.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import Select from '../ui/Select.svelte';
  import {
    healthToDisabledReason,
    problemToDisabledReason,
  } from '../output-review/model';

  type Mode = components['schemas']['PromptAssistMode'];
  type Response = components['schemas']['PromptAssistResponse'];

  let {
    workflowId,
    parameter,
    value,
    positivePrompt,
    negative = false,
    disabled = false,
    accept,
  }: {
    workflowId: string;
    parameter: string;
    value: string;
    positivePrompt?: string;
    negative?: boolean;
    disabled?: boolean;
    accept: (value: string) => void;
  } = $props();

  let availability = $state<'checking' | 'available' | 'unavailable'>(
    'checking',
  );
  let disabledReason = $state<string>();
  let settingsHint = $state(false);
  let healthProblem = $state<Problem>();
  let open = $state(false);
  let mode = $state<Mode>('expand');
  let instruction = $state('');
  let count = $state('4');
  let pending = $state(false);
  let requestSnapshot = $state('');
  let result = $state<Response>();
  let requestProblem = $state<Problem>();
  let lastApplied = $state<{ from: string; to: string }>();
  let controller: AbortController | undefined;
  let healthController: AbortController | undefined;
  let nextRequest = 0;

  const stale = $derived(!!result && value !== requestSnapshot);
  const busy = $derived(availability === 'checking' || pending);
  const unavailable = $derived(
    disabled || availability !== 'available' || !!disabledReason,
  );
  const canUndo = $derived(
    !!lastApplied && value === lastApplied.to && !disabled,
  );

  onDestroy(() => {
    controller?.abort();
    healthController?.abort();
  });

  $effect(() => {
    if (!open) {
      controller?.abort();
      controller = undefined;
    }
  });

  async function checkAvailability(signal?: AbortSignal) {
    healthController?.abort();
    const current = new AbortController();
    healthController = current;
    if (signal)
      signal.addEventListener('abort', () => current.abort(), { once: true });
    availability = 'checking';
    disabledReason = undefined;
    settingsHint = false;
    healthProblem = undefined;
    try {
      const health = await api.health({ signal: current.signal });
      if (current.signal.aborted) return;
      const reason = healthToDisabledReason(health, 'text');
      availability = reason ? 'unavailable' : 'available';
      disabledReason = reason;
      settingsHint = !!reason;
    } catch (error) {
      if (current.signal.aborted) return;
      const problem = asProblem(error, 'Could not check prompt assistance');
      availability = 'unavailable';
      disabledReason =
        problemToDisabledReason(
          problem,
          'The configured text model is unavailable for prompt assistance.',
        ) ?? 'Could not check prompt assistance. Check Settings and try again.';
      settingsHint = true;
      healthProblem = problem;
    }
  }

  $effect(() => {
    const lifecycle = new AbortController();
    void checkAvailability(lifecycle.signal);
    return () => lifecycle.abort();
  });

  function asProblem(error: unknown, title: string): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  async function choose(next: Mode) {
    if (unavailable) return;
    mode = next;
    instruction = '';
    count = '4';
    result = undefined;
    requestProblem = undefined;
    open = true;
    await tick();
    if (next === 'expand' || next === 'negative') await propose();
  }

  async function propose() {
    if (pending || unavailable) return;
    if (mode === 'refine' && !instruction.trim()) return;
    controller?.abort();
    const active = new AbortController();
    controller = active;
    const requestId = ++nextRequest;
    requestSnapshot = value;
    pending = true;
    result = undefined;
    requestProblem = undefined;
    const body: PromptAssistRequest = {
      mode,
      prompt: mode === 'negative' ? (positivePrompt ?? value) : value,
      workflow_id: workflowId,
      parameter,
    };
    if (mode === 'refine') body.instruction = instruction;
    if (mode === 'variations') body.n = Number(count);
    if (mode === 'negative') body.original = value;
    try {
      const response = await api.promptAssist(body, { signal: active.signal });
      if (active.signal.aborted || requestId !== nextRequest || !open) return;
      if (response.mode !== mode)
        throw new Error(
          'The prompt assist response did not match the request.',
        );
      result = response;
    } catch (error) {
      if (active.signal.aborted || requestId !== nextRequest) return;
      const problem = asProblem(error, 'Could not propose a prompt');
      requestProblem = problem;
      const reason = problemToDisabledReason(
        problem,
        'The configured text model is unavailable for prompt assistance.',
      );
      if (reason) {
        availability = 'unavailable';
        disabledReason = reason;
        settingsHint = true;
      }
    } finally {
      if (requestId === nextRequest) pending = false;
    }
  }

  function acceptProposal(text: string) {
    if (pending || stale || !open) return;
    lastApplied = { from: requestSnapshot, to: text };
    accept(text);
    open = false;
  }

  function discard() {
    controller?.abort();
    open = false;
    result = undefined;
    requestProblem = undefined;
  }

  function undo() {
    if (!lastApplied || !canUndo) return;
    const previous = lastApplied.from;
    lastApplied = undefined;
    accept(previous);
  }
</script>

<div class="prompt-assist" aria-label={`Prompt assistance for ${parameter}`}>
  <div class="actions">
    <Button
      onclick={() => choose('expand')}
      disabled={unavailable}
      aria-busy={busy}>Expand</Button
    >
    <Button onclick={() => choose('refine')} disabled={unavailable}
      >Refine</Button
    >
    {#if negative}
      <Button onclick={() => choose('negative')} disabled={unavailable}
        >Negative</Button
      >
    {/if}
    <Button onclick={() => choose('variations')} disabled={unavailable}
      >Variations</Button
    >
    {#if canUndo}<Button onclick={undo}>Undo</Button>{/if}
  </div>
  {#if availability === 'checking'}
    <p role="status">Checking prompt assistance availability…</p>
  {:else if disabledReason}
    <p class="disabled-reason">
      Prompt assistance is disabled. {disabledReason}
      {#if settingsHint}Check <a href="#/settings">Settings</a> for a configured text
        model.{/if}
    </p>
    <Button onclick={() => checkAvailability()}>Check availability again</Button
    >
  {/if}
  {#if healthProblem}<ProblemDetails problem={healthProblem} />{/if}
</div>

<Dialog
  bind:open
  title={`${negative ? 'Negative prompt' : 'Prompt'} ${mode}`}
  description="Model output is a proposal. Review the diff, then explicitly accept one proposal or discard it."
>
  <div class="assist-dialog" aria-busy={pending}>
    {#if mode === 'refine'}
      <Input
        label="Refinement instruction"
        hint="Describe how the current prompt should change."
        multiline
        bind:value={instruction}
      />
    {:else if mode === 'variations'}
      <Select
        label="Number of variations"
        bind:value={count}
        options={Array.from({ length: 8 }, (_, index) => ({
          value: String(index + 1),
          label: String(index + 1),
        }))}
      />
    {/if}
    {#if !result}
      <Button
        variant="primary"
        onclick={propose}
        disabled={pending ||
          unavailable ||
          (mode === 'refine' && !instruction.trim())}
        aria-busy={pending}>{pending ? 'Proposing…' : 'Propose'}</Button
      >
    {/if}
    {#if pending}<p role="status">Requesting prompt proposals…</p>{/if}
    {#if requestProblem}<ProblemDetails problem={requestProblem} />{/if}
    {#if disabledReason}<p class="disabled-reason">
        Prompt assistance is disabled. {disabledReason} Check
        <a href="#/settings">Settings</a> for a configured text model.
      </p>{/if}
    {#if result}
      <section aria-live="polite" aria-label="Prompt proposals">
        <p>
          {result.proposal_count} proposal{result.proposal_count === 1
            ? ''
            : 's'} from {result.model}.
        </p>
        {#if stale}<p class="stale" role="status">
            The field changed while assistance was pending or being reviewed. No
            proposal can be applied to that changed value. Rerun assistance
            first.
          </p>{/if}
        <ol class="proposals">
          {#each result.proposals as proposal, index (index)}
            <li>
              <h3>Proposal {index + 1}</h3>
              <p class="proposal-text">{proposal.text}</p>
              <p class="diff" aria-label={`Proposal ${index + 1} diff`}>
                {#each proposal.diff as part, partIndex (partIndex)}
                  {#if part.op === 'insert'}
                    <ins><span class="sr-only">Inserted: </span>{part.text}</ins
                    >
                  {:else if part.op === 'delete'}
                    <del><span class="sr-only">Deleted: </span>{part.text}</del>
                  {:else}
                    <span>{part.text}</span>
                  {/if}
                {/each}
              </p>
              <Button
                variant="primary"
                onclick={() => acceptProposal(proposal.text)}
                disabled={stale || pending}>Accept</Button
              >
            </li>
          {/each}
        </ol>
        {#if stale}<Button onclick={propose} disabled={pending}
            >Rerun assistance</Button
          >{/if}
      </section>
    {/if}
    <Button onclick={discard}>Discard</Button>
  </div>
</Dialog>

<style>
  .prompt-assist,
  .assist-dialog,
  .proposals li {
    display: grid;
    gap: var(--space-2);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .prompt-assist p,
  .proposal-text,
  .diff {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .disabled-reason,
  .stale {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .proposals {
    display: grid;
    gap: var(--space-3);
    padding-inline-start: 1.5rem;
  }
  .proposals li {
    min-width: 0;
  }
  ins {
    background: color-mix(in srgb, var(--success, green) 18%, transparent);
    text-decoration: underline;
  }
  del {
    background: color-mix(in srgb, var(--danger) 18%, transparent);
  }
  .assist-dialog :global(.ui-field) {
    width: min(100%, 32rem);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }
</style>
