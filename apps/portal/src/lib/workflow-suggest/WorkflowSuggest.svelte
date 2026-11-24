<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api, ApiProblemError, BindingFloat } from '../api/client';
  import type { components } from '../api/schema';
  import Button from '../ui/Button.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import Input from '../ui/Input.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import {
    healthToDisabledReason,
    problemToDisabledReason,
  } from '../output-review/model';
  import { buildSuggestionGenerateUrl } from '../../routes/generate/share';

  type Response = components['schemas']['SuggestWorkflowResponse'];

  let open = $state(false);
  let intent = $state('');
  let availability = $state<'idle' | 'checking' | 'available' | 'unavailable'>(
    'idle',
  );
  let disabledReason = $state<string>();
  let settingsHint = $state(false);
  let pending = $state(false);
  let result = $state<Response>();
  let problem = $state<Problem>();
  let requestIntent = $state('');
  let requestId = 0;
  let controller: AbortController | undefined;
  let healthController: AbortController | undefined;
  const stale = $derived(!!result && requestIntent !== intent);
  const busy = $derived(availability === 'checking' || pending);
  const preparedSuggestions = $derived.by(() =>
    (result?.suggestions ?? []).map((suggestion) => {
      try {
        return { suggestion, href: buildSuggestionGenerateUrl(suggestion) };
      } catch (error) {
        return {
          suggestion,
          error:
            error instanceof Error
              ? error.message
              : 'The Generate link could not be created.',
        };
      }
    }),
  );

  onDestroy(() => {
    controller?.abort();
    healthController?.abort();
  });

  $effect(() => {
    if (open) {
      void checkAvailability();
    } else {
      controller?.abort();
      controller = undefined;
      healthController?.abort();
      pending = false;
    }
  });

  function asProblem(error: unknown, title: string): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  async function checkAvailability() {
    healthController?.abort();
    const active = new AbortController();
    healthController = active;
    availability = 'checking';
    disabledReason = undefined;
    settingsHint = false;
    problem = undefined;
    try {
      const health = await api.health({ signal: active.signal });
      if (active.signal.aborted || !open) return;
      const reason = healthToDisabledReason(health, 'text');
      availability = reason ? 'unavailable' : 'available';
      disabledReason = reason;
      settingsHint = !!reason;
    } catch (error) {
      if (active.signal.aborted || !open) return;
      const failure = asProblem(error, 'Could not check workflow suggestions');
      availability = 'unavailable';
      disabledReason =
        problemToDisabledReason(
          failure,
          'The configured text model is unavailable for workflow suggestions.',
        ) ??
        'Could not check workflow suggestions. Check Settings and try again.';
      settingsHint = true;
      problem = failure;
    }
  }

  async function suggest(event: SubmitEvent) {
    event.preventDefault();
    if (pending || !intent.trim() || availability !== 'available') return;
    controller?.abort();
    const active = new AbortController();
    controller = active;
    const id = ++requestId;
    requestIntent = intent;
    pending = true;
    result = undefined;
    problem = undefined;
    try {
      const response = await api.suggestWorkflow(
        { intent },
        { signal: active.signal },
      );
      if (active.signal.aborted || id !== requestId || !open) return;
      result = response;
    } catch (error) {
      if (active.signal.aborted || id !== requestId || !open) return;
      const failure = asProblem(error, 'Could not suggest a workflow');
      problem = failure;
      const reason = problemToDisabledReason(
        failure,
        'The configured text model is unavailable for workflow suggestions.',
      );
      if (reason) {
        availability = 'unavailable';
        disabledReason = reason;
        settingsHint = true;
      }
    } finally {
      if (id === requestId) pending = false;
    }
  }

  function discard() {
    controller?.abort();
    result = undefined;
    problem = undefined;
    open = false;
  }

  function display(value: unknown) {
    if (value instanceof BindingFloat || typeof value === 'bigint')
      return value.toString();
    if (typeof value === 'number' && Object.is(value, -0)) return '-0.0';
    return typeof value === 'string' ? JSON.stringify(value) : String(value);
  }
</script>

<Button onclick={() => (open = true)}>Suggest workflow</Button>

<Dialog
  bind:open
  title="Suggest workflow"
  description="Describe the result you want. Suggestions prefill Generate for review; they never submit a run."
>
  <div class="suggest" aria-busy={busy}>
    {#if availability === 'checking'}
      <p role="status">Checking text model availability…</p>
    {:else if disabledReason}
      <p class="disabled-reason">
        Workflow suggestions are disabled. {disabledReason}
        {#if settingsHint}Check <a href="#/settings">Settings</a> for a configured
          text model.{/if}
      </p>
      <Button onclick={checkAvailability}>Check availability again</Button>
    {/if}
    {#if problem}<ProblemDetails {problem} />{/if}
    <form onsubmit={suggest}>
      <Input
        label="What do you want to create?"
        hint="Describe the desired output and useful constraints."
        multiline
        bind:value={intent}
        disabled={availability !== 'available'}
      />
      <Button
        type="submit"
        variant="primary"
        disabled={availability !== 'available' || pending || !intent.trim()}
        aria-busy={pending}>{pending ? 'Suggesting…' : 'Suggest'}</Button
      >
    </form>
    {#if pending}<p role="status">Ranking workflows for this intent…</p>{/if}
    {#if result}
      <section aria-live="polite" aria-label="Workflow suggestions">
        <p>
          {result.suggestions.length} suggestion{result.suggestions.length === 1
            ? ''
            : 's'} from {result.model}.
        </p>
        {#if stale}<p class="stale" role="status">
            The intent changed after these suggestions were requested. Suggest
            again before choosing one.
          </p>{/if}
        {#if result.suggestions.length === 0}<p>
            No workflows matched this intent.
          </p>{/if}
        <ol class="suggestions">
          {#each preparedSuggestions as prepared (prepared.suggestion.workflow_id)}
            {@const suggestion = prepared.suggestion}
            <li>
              <div>
                <h3>#{suggestion.rank} {suggestion.workflow_id}</h3>
                <p>{suggestion.rationale}</p>
                <p class="revision">
                  Revision: <code>{suggestion.workflow_revision}</code>
                </p>
              </div>
              <dl>
                {#each Object.entries(suggestion.parameters) as [name, value] (name)}
                  <dt>{name}</dt>
                  <dd><code>{display(value)}</code></dd>
                {/each}
              </dl>
              {#if Object.keys(suggestion.parameter_notes ?? {}).length}
                <div>
                  <h4>Parameter notes</h4>
                  <ul>
                    {#each Object.entries(suggestion.parameter_notes ?? {}) as [name, note] (name)}
                      <li><strong>{name}:</strong> {note}</li>
                    {/each}
                  </ul>
                </div>
              {/if}
              {#if prepared.href}
                <a
                  class="ui-button primary"
                  href={prepared.href}
                  aria-disabled={stale}
                  onclick={(event) => {
                    if (stale) event.preventDefault();
                  }}>Use this suggestion</a
                >
              {:else}
                <Button variant="primary" disabled>Use this suggestion</Button>
                <p class="disabled-reason" role="status">
                  This suggestion cannot be opened in Generate: {prepared.error}
                </p>
              {/if}
            </li>
          {/each}
        </ol>
        {#if result.dropped.length}
          <section aria-labelledby="dropped-workflows">
            <h3 id="dropped-workflows">Suggestions not shown</h3>
            <ul>
              {#each result.dropped as dropped, index (index)}
                <li>
                  <strong>{dropped.workflow_id} / {dropped.field}:</strong>
                  {dropped.reason}
                </li>
              {/each}
            </ul>
          </section>
        {/if}
        {#if result.truncated}<p role="status">
            More workflows matched than were returned. Broaden or refine the
            request as needed.
          </p>{/if}
        <p class="note">
          Image inputs are not suggested. Choose any required images yourself in
          Generate.
        </p>
      </section>
    {/if}
    <Button onclick={discard}>Discard</Button>
  </div>
</Dialog>

<style>
  .suggest,
  .suggest form,
  .suggestions li,
  .suggestions li > div {
    display: grid;
    gap: var(--space-2);
  }
  .suggest form :global(.ui-field) {
    width: 100%;
  }
  .suggestions {
    display: grid;
    gap: var(--space-3);
    padding-inline-start: 1.5rem;
  }
  .suggestions li {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
  }
  h3,
  h4,
  p,
  dl,
  ul {
    margin-block: 0;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(8rem, max-content) 1fr;
    gap: var(--space-1) var(--space-3);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .disabled-reason,
  .stale,
  .note,
  .revision {
    color: var(--muted);
    font-size: var(--text-small);
  }
</style>
