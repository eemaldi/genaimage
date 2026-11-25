<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import {
    api,
    ApiProblemError,
    formatExactJson,
    parseAssistantJson,
    type AssistantStream,
  } from '../api/client';
  import Button from '../ui/Button.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import { asRecord, asRunProposal, generateAction } from './card-utils';
  import type { Confirmation } from './model';

  let {
    confirmation,
    conversationId,
    onstream,
  }: {
    confirmation: Confirmation;
    conversationId: string;
    onstream?: (stream: AssistantStream) => Promise<void>;
  } = $props();

  type SchemaField = {
    name: string;
    required: boolean;
    type: 'string' | 'number' | 'integer' | 'boolean' | 'object' | 'array';
  };

  const uid = $props.id();
  const initialConfirmation = untrack(() => confirmation);
  const fallbackSchema = {
    type: 'object',
    required: ['workflow_id', 'workflow_revision', 'parameters'],
    properties: {
      workflow_id: { type: 'string' },
      workflow_revision: { type: 'string' },
      preset: { type: 'string' },
      parameters: { type: 'object' },
    },
  };
  const controller = new AbortController();
  let status = $state(initialConfirmation.status);
  let preview = $state<unknown>(initialConfirmation.preview);
  let previewToken = $state(initialConfirmation.preview_token);
  let invocationResult = $state<unknown>(initialConfirmation.invocation_result);
  let invocationError = $state(
    initialConfirmation.invocation_error ??
      initialConfirmation.validation_error ??
      '',
  );
  let busy = $state(false);
  let problem = $state<Problem>();
  let workflowChanged = $state(false);
  let editOpen = $state(false);
  let rejectOpen = $state(false);
  let rejectReason = $state('');
  let editorText = $state('');
  let editorFallback = $state(false);
  let fieldDrafts = $state<Record<string, string>>({});
  let editorProblem = $state<Problem>();
  let editorBusy = $state(false);
  let editPreview = $state<unknown>();
  let editPreviewToken = $state('');
  let reviewedArguments = $state<unknown>();
  let reviewedFingerprint = $state('');
  let previewStale = $state(false);
  let announcement = $state('');

  const proposal = $derived(asRunProposal(preview));
  const generated = $derived(proposal ? generateAction(proposal) : undefined);
  const issues = $derived.by(() => {
    const record = asRecord(preview);
    return Array.isArray(record?.issues) ? record.issues : [];
  });
  const pending = $derived(status === 'pending');
  const schema = $derived(
    asRecord(confirmation.json_schema) ??
      (confirmation.tool === 'submit_run' ? fallbackSchema : undefined),
  );
  const schemaFields = $derived.by(() => {
    const properties = asRecord(schema?.properties);
    if (!properties || schema?.type !== 'object') return [] as SchemaField[];
    const required = Array.isArray(schema.required)
      ? new Set(
          schema.required.filter(
            (item): item is string => typeof item === 'string',
          ),
        )
      : new Set<string>();
    return Object.entries(properties).flatMap(([name, definition]) => {
      const record = asRecord(definition);
      const type = record?.type;
      return [
        'string',
        'number',
        'integer',
        'boolean',
        'object',
        'array',
      ].includes(String(type))
        ? [
            {
              name,
              required: required.has(name),
              type: type as SchemaField['type'],
            },
          ]
        : [];
    });
  });
  const runId = $derived.by(() => {
    const result = asRecord(invocationResult);
    return typeof result?.run_id === 'string' ? result.run_id : undefined;
  });
  const editFingerprint = $derived.by(() => {
    try {
      return formatExactJson(composeArguments(), 0);
    } catch {
      return '';
    }
  });
  const editReady = $derived(
    !!editPreviewToken &&
      !!reviewedFingerprint &&
      editFingerprint === reviewedFingerprint,
  );

  onMount(() => {
    if (pending && (!previewToken || preview === undefined))
      void refreshPersistedPreview();
  });
  onDestroy(() => controller.abort());

  $effect(() => {
    if (!editOpen) return;
    void tick().then(() =>
      document
        .getElementById(
          `${uid}-editor${editorFallback ? '-json' : `-${schemaFields[0]?.name}`}`,
        )
        ?.focus(),
    );
  });

  function asProblem(error: unknown, title: string): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  function displayIssue(issue: unknown) {
    try {
      return typeof issue === 'string' ? issue : formatExactJson(issue);
    } catch {
      return String(issue);
    }
  }

  function applyReplacement(failure: Problem) {
    if (failure.preview !== undefined) {
      preview = failure.preview;
      previewToken = failure.preview_token ?? '';
      workflowChanged = true;
      announcement =
        'The workflow changed. Review the replacement preview and confirm it explicitly.';
    }
  }

  async function refreshPersistedPreview() {
    busy = true;
    try {
      const response = await api.previewAssistantConfirmation(
        conversationId,
        initialConfirmation.confirmation_id,
        { arguments: initialConfirmation.arguments },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      preview = response.preview;
      previewToken = response.preview_token;
      problem = undefined;
    } catch (error) {
      if (controller.signal.aborted) return;
      problem = asProblem(error, 'Could not restore the confirmation preview');
      applyReplacement(problem);
    } finally {
      busy = false;
    }
  }

  function openEditor() {
    editorText = formatExactJson(confirmation.arguments);
    fieldDrafts = {};
    editPreview = undefined;
    editPreviewToken = '';
    reviewedArguments = undefined;
    reviewedFingerprint = '';
    previewStale = false;
    editorProblem = undefined;
    editorFallback = schemaFields.length === 0;
    editOpen = true;
  }

  function fieldValue(name: string) {
    if (Object.hasOwn(fieldDrafts, name)) return fieldDrafts[name];
    const argumentsRecord = asRecord(confirmation.arguments);
    const value = argumentsRecord?.[name];
    if (value === undefined) return '';
    return typeof value === 'string' || typeof value === 'boolean'
      ? String(value)
      : formatExactJson(value, 0);
  }

  function updateField(name: string, value: string) {
    fieldDrafts = { ...fieldDrafts, [name]: value };
    invalidateEditPreview();
  }

  function updateJson(value: string) {
    editorText = value;
    invalidateEditPreview();
  }

  function invalidateEditPreview() {
    previewStale = !!reviewedFingerprint;
    editPreviewToken = '';
    reviewedArguments = undefined;
    editPreview = undefined;
    editorProblem = undefined;
  }

  function composeArguments(): unknown {
    if (editorFallback) return parseAssistantJson(editorText);
    const original = parseAssistantJson(editorText);
    const record = asRecord(original);
    if (!record) throw new Error('Arguments must be a JSON object.');
    const values: Record<string, unknown> = { ...record };
    for (const field of schemaFields) {
      if (!Object.hasOwn(fieldDrafts, field.name)) continue;
      const draft = fieldDrafts[field.name];
      if (field.type === 'string') {
        if (!draft && !field.required) delete values[field.name];
        else values[field.name] = draft;
      } else if (!draft) {
        if (field.required) throw new Error(`${field.name} is required.`);
        delete values[field.name];
      } else {
        const parsed = parseAssistantJson(draft);
        if (field.type === 'boolean' && typeof parsed !== 'boolean')
          throw new Error(`${field.name} must be true or false.`);
        values[field.name] = parsed;
      }
    }
    return values;
  }

  function showJsonEditor() {
    try {
      editorText = formatExactJson(composeArguments());
      fieldDrafts = {};
      editorFallback = true;
      invalidateEditPreview();
      void tick().then(() =>
        document.getElementById(`${uid}-editor-json`)?.focus(),
      );
    } catch (error) {
      editorProblem = {
        title: 'Could not open the JSON editor',
        detail: error instanceof Error ? error.message : undefined,
        recovery: 'Correct the highlighted field value and try again.',
      };
    }
  }

  async function previewEdit() {
    if (editorBusy || status !== 'pending') return;
    editorBusy = true;
    editorProblem = undefined;
    try {
      const argumentsValue = composeArguments();
      const fingerprint = formatExactJson(argumentsValue, 0);
      const response = await api.previewAssistantConfirmation(
        conversationId,
        initialConfirmation.confirmation_id,
        { arguments: argumentsValue },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      editPreview = response.preview;
      editPreviewToken = response.preview_token;
      reviewedArguments = argumentsValue;
      reviewedFingerprint = fingerprint;
      previewStale = false;
      announcement =
        'Edited arguments previewed. Review every effective value.';
    } catch (error) {
      if (controller.signal.aborted) return;
      editPreviewToken = '';
      reviewedArguments = undefined;
      editorProblem = asProblem(
        error,
        'Could not preview the edited arguments',
      );
      if (
        error instanceof ApiProblemError &&
        (error.status === 409 || error.status === 422)
      )
        applyReplacement(editorProblem);
    } finally {
      editorBusy = false;
    }
  }

  async function consume(stream: AssistantStream) {
    if (onstream) return onstream(stream);
    for await (const frame of stream) {
      if (frame.event === 'tool_result' && 'tool_call_id' in frame.data) {
        const data = frame.data as {
          tool_call_id: string;
          ok: boolean;
          result?: unknown;
          error?: string;
        };
        if (data.tool_call_id !== confirmation.tool_call_id) continue;
        invocationResult = data.result;
        invocationError = data.error ?? '';
      } else if (frame.event === 'error' && 'error' in frame.data) {
        invocationError = String(
          frame.data.error ?? 'The tool invocation failed.',
        );
      }
    }
  }

  async function syncPersistedState() {
    try {
      const audit = await api.listAssistantToolCalls(conversationId, 200, {
        signal: controller.signal,
      });
      const record = audit.tool_calls.find(
        ({ confirmation_id }) =>
          confirmation_id === confirmation.confirmation_id,
      );
      if (!record?.confirmation_status) return;
      const next = record.confirmation_status;
      if (
        [
          'pending',
          'invoking',
          'succeeded',
          'failed',
          'rejected',
          'outcome_unknown',
          'expired',
        ].includes(next)
      )
        status = next as typeof status;
      invocationResult = record.invocation_result;
      invocationError =
        record.invocation_error ?? record.validation_error ?? '';
    } catch {
      // The in-memory invoking state remains conservative until the next load.
    }
  }

  async function decide(body: {
    decision: 'confirm' | 'edit' | 'reject';
    preview_token?: string;
    arguments?: unknown;
    reason?: string;
  }) {
    if (busy || !pending) return;
    busy = true;
    problem = undefined;
    workflowChanged = false;
    try {
      const stream = await api.decideAssistantConfirmation(
        conversationId,
        initialConfirmation.confirmation_id,
        body,
        { signal: controller.signal },
      );
      status = 'invoking';
      announcement = body.decision === 'reject' ? 'Rejecting…' : 'Submitting…';
      await consume(stream);
      await syncPersistedState();
      editOpen = false;
      rejectOpen = false;
      const finalStatus = String(status) as Confirmation['status'];
      announcement =
        finalStatus === 'succeeded'
          ? 'The backend confirmed the submitted run.'
          : finalStatus === 'rejected'
            ? 'The proposed run was rejected.'
            : 'The server response was received.';
    } catch (error) {
      status = 'pending';
      problem = asProblem(
        error,
        body.decision === 'reject'
          ? 'Could not reject the proposal'
          : 'Could not submit the decision',
      );
      if (
        error instanceof ApiProblemError &&
        (error.status === 409 || error.status === 422)
      )
        applyReplacement(problem);
      announcement =
        problem.detail ?? problem.title ?? 'The decision was not accepted.';
    } finally {
      busy = false;
    }
  }

  function confirm() {
    if (!previewToken) return;
    void decide({ decision: 'confirm', preview_token: previewToken });
  }

  function confirmEdit() {
    if (!editReady || !reviewedArguments) return;
    void decide({
      decision: 'edit',
      arguments: reviewedArguments,
      preview_token: editPreviewToken,
    });
  }

  function reject() {
    void decide({
      decision: 'reject',
      reason: rejectReason.trim() || undefined,
    });
  }
</script>

<section
  class:needs-attention={workflowChanged}
  class="confirmation-card"
  aria-labelledby={`${uid}-title`}
>
  <header>
    <div>
      <p class="eyebrow">Run confirmation</p>
      <h3 id={`${uid}-title`}>{confirmation.tool}</h3>
    </div>
    <span
      class:danger={status === 'failed' || status === 'outcome_unknown'}
      class="status">{status}</span
    >
  </header>

  <div class="confirmation-body">
    {#if status === 'pending'}
      <p class="not-submitted">
        <strong>Not submitted.</strong> Review every effective value before confirming.
      </p>
    {:else if status === 'invoking'}
      <p role="status">Submitting…</p>
    {:else if status === 'succeeded' && runId}
      <p class="success" role="status">
        The backend confirmed the submitted run.
      </p>
      <a class="ui-button primary" href={`#/runs/${encodeURIComponent(runId)}`}
        >Open run {runId}</a
      >
    {:else if status === 'succeeded'}
      <p role="status">
        The backend marked the invocation succeeded, but no run ID is available.
        Check Jobs.
      </p>
    {:else if status === 'failed'}
      <p class="error-text" role="alert">
        The invocation failed. {invocationError}
      </p>
    {:else if status === 'outcome_unknown'}
      <p class="unknown" role="alert">
        Outcome unknown — check Jobs / reconcile. This is not shown as success.
      </p>
    {:else if status === 'rejected'}
      <p role="status">Rejected. The run was not submitted.</p>
    {:else if status === 'expired'}
      <p role="status">Expired. The run was not submitted.</p>
    {/if}

    {#if workflowChanged}
      <div class="changed" role="alert">
        <strong>The workflow changed.</strong> The replacement preview below has a
        fresh token and must be explicitly confirmed again. No run has been submitted
        by this change.
      </div>
    {/if}

    {#if proposal}
      <dl class="pinned">
        <dt>Workflow</dt>
        <dd><code>{proposal.workflow_id}</code></dd>
        <dt>Pinned revision</dt>
        <dd><code>{proposal.workflow_revision}</code></dd>
        <dt>Preset</dt>
        <dd><code>{proposal.preset ?? '(none)'}</code></dd>
      </dl>
      <div>
        <p class="label">Every effective value</p>
        <dl class="parameters">
          {#each Object.entries(proposal.parameters) as [name, value] (name)}
            <dt>{name}</dt>
            <dd><code>{formatExactJson(value, 0)}</code></dd>
          {/each}
        </dl>
      </div>
    {:else if preview !== undefined}
      <p class="label">Preview</p>
      <pre><code>{formatExactJson(preview)}</code></pre>
    {/if}

    {#if issues.length}
      <div class="issues" role="alert">
        <strong>Issues</strong>
        <ul>
          {#each issues as issue, index (index)}<li>
              {displayIssue(issue)}
            </li>{/each}
        </ul>
      </div>
    {/if}

    {#if generated?.href}
      <a class="ui-button" href={generated.href}>Open in Generate</a>
      <p class="note">This only prefills Generate. It does not submit a run.</p>
    {:else if generated?.error}
      <Button disabled>Open in Generate</Button>
      <p class="disabled-reason" role="status">
        Open in Generate is unavailable: {generated.error}
      </p>
    {/if}

    {#if problem}<ProblemDetails {problem} />{/if}
    {#if pending}
      <div class="actions">
        <Button disabled={busy} onclick={openEditor}>Edit</Button>
        <Button disabled={busy} onclick={() => (rejectOpen = true)}
          >Reject</Button
        >
        <Button
          variant="primary"
          disabled={busy || !previewToken}
          aria-busy={busy}
          onclick={confirm}
        >
          {workflowChanged ? 'Confirm replacement preview' : 'Confirm run'}
        </Button>
      </div>
    {/if}
  </div>
  <span class="visually-hidden" aria-live="polite">{announcement}</span>
</section>

<Dialog
  bind:open={editOpen}
  title="Edit proposed run arguments"
  description="Preview the edit and review every effective value before confirming it."
>
  <div class="editor" aria-busy={editorBusy}>
    {#if editorProblem}<ProblemDetails problem={editorProblem} />{/if}
    {#if editorFallback}
      <label class="editor-label" for={`${uid}-editor-json`}
        >Arguments (exact JSON)</label
      >
      <textarea
        id={`${uid}-editor-json`}
        rows="12"
        class="ui-control"
        value={editorText}
        oninput={(event) => updateJson(event.currentTarget.value)}
      ></textarea>
    {:else}
      {#each schemaFields as field (field.name)}
        <label class="editor-label" for={`${uid}-editor-${field.name}`}
          >{field.name}{field.required ? ' (required)' : ''}</label
        >
        {#if field.type === 'object' || field.type === 'array'}
          <textarea
            id={`${uid}-editor-${field.name}`}
            rows="6"
            class="ui-control"
            value={fieldValue(field.name)}
            oninput={(event) =>
              updateField(field.name, event.currentTarget.value)}
          ></textarea>
        {:else if field.type === 'boolean'}
          <select
            id={`${uid}-editor-${field.name}`}
            class="ui-control"
            value={fieldValue(field.name)}
            onchange={(event) =>
              updateField(field.name, event.currentTarget.value)}
          >
            <option value="">Unset</option><option value="true">true</option
            ><option value="false">false</option>
          </select>
        {:else}
          <input
            id={`${uid}-editor-${field.name}`}
            class="ui-control"
            value={fieldValue(field.name)}
            oninput={(event) =>
              updateField(field.name, event.currentTarget.value)}
          />
        {/if}
      {/each}
      <Button onclick={showJsonEditor}>Use exact JSON editor</Button>
    {/if}
    <Button disabled={editorBusy} aria-busy={editorBusy} onclick={previewEdit}
      >{editorBusy ? 'Previewing…' : 'Preview edit'}</Button
    >

    {#if editPreview !== undefined}
      <section
        class="edit-preview"
        aria-live="polite"
        aria-label="Edited effective values"
      >
        <p>
          <strong>Edited preview.</strong> These values have not been submitted.
        </p>
        <pre><code>{formatExactJson(editPreview)}</code></pre>
      </section>
    {/if}
    {#if previewStale && !editReady}
      <p class="disabled-reason" role="status">
        The arguments changed after preview. Preview the edit again before
        confirming.
      </p>
    {/if}
    <div class="actions">
      <Button onclick={() => (editOpen = false)}>Cancel edit</Button>
      <Button
        variant="primary"
        disabled={!editReady || busy}
        onclick={confirmEdit}>Confirm edit</Button
      >
    </div>
  </div>
</Dialog>

<Dialog
  bind:open={rejectOpen}
  title="Reject proposed run"
  description="Rejecting leaves the confirmation decided and does not submit a run."
>
  <label class="editor-label" for={`${uid}-reject-reason`}
    >Reason (optional)</label
  >
  <textarea
    id={`${uid}-reject-reason`}
    rows="4"
    class="ui-control"
    bind:value={rejectReason}
  ></textarea>
  <div class="actions">
    <Button onclick={() => (rejectOpen = false)}>Keep reviewing</Button>
    <Button disabled={busy} onclick={reject}>Reject proposal</Button>
  </div>
</Dialog>

<style>
  .confirmation-card {
    display: grid;
    gap: var(--space-3);
    border: 1px solid var(--accent);
    border-radius: var(--radius-panel);
    overflow: hidden;
  }
  .needs-attention {
    border-color: var(--danger);
  }
  header,
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  header {
    justify-content: space-between;
    padding: var(--space-3);
    background: var(--subtle);
  }
  header h3,
  header p,
  .confirmation-body > p,
  .editor > p {
    margin: 0;
  }
  .confirmation-body,
  .editor {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-3);
  }
  .eyebrow,
  .label,
  .note,
  .disabled-reason {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .status {
    text-transform: capitalize;
  }
  .danger,
  .error-text {
    color: var(--danger);
  }
  .not-submitted,
  .changed,
  .edit-preview {
    padding: var(--space-2);
    background: var(--subtle);
    border-radius: var(--radius-control);
  }
  .changed,
  .issues {
    border: 1px solid var(--danger);
  }
  .changed {
    display: grid;
    gap: var(--space-1);
  }
  .issues {
    padding: var(--space-2);
    border-radius: var(--radius-control);
  }
  .issues ul {
    margin: var(--space-1) 0 0;
    padding-inline-start: 1.25rem;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(8rem, max-content) minmax(0, 1fr);
    gap: var(--space-1) var(--space-3);
    margin: 0;
  }
  dt {
    font-weight: var(--weight-strong);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  pre {
    margin: 0;
    max-inline-size: 100%;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .actions {
    justify-content: flex-end;
  }
  .editor-label {
    font-weight: var(--weight-strong);
  }
  .editor textarea,
  .editor input,
  .editor select {
    inline-size: 100%;
  }
  .unknown {
    color: var(--danger);
    font-weight: var(--weight-strong);
  }
</style>
