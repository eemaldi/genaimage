<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import {
    api,
    ApiProblemError,
    type SubmitRunRequest,
    BindingFloat,
    supportsExactNumberWrites,
  } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import {
    displayScalar,
    encodeFormState,
    restoreFormState,
    type PendingSuggestion,
    type SharedInherited,
  } from './share';
  import type { PendingSubmission } from './pending';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ParameterField from './ParameterField.svelte';
  import {
    bindingError,
    initialField,
    isNegativePromptParameter,
    isPromptParameter,
    parameterGroups,
    presetField,
    presenceLabel,
    requirementLabel,
    randomSeed,
    type FieldState,
    type Review,
    type Workflow,
  } from './model';

  type Snapshot = {
    body: Readonly<SubmitRunRequest & { workflow_revision: string }>;
    rows: ReadonlyArray<{
      name: string;
      value?: Review[string];
      source?: FieldState['source'];
      random: boolean;
      image: boolean;
      requirement: string;
      sharedDefault: boolean;
    }>;
    inherited: Map<string, SharedInherited>;
    inheritedChanges: string[];
    outputs: ReadonlyArray<{ node: string; kind: string }>;
    workflow: string;
    version: string;
    preset: string;
    modified: boolean;
    key: string;
  };
  let {
    workflow,
    query,
    reloadWorkflow,
    editingLocked,
    accepted,
    submit,
    resetSubmission,
  }: {
    editingLocked: boolean;
    accepted: boolean;
    submit: (
      snapshot: PendingSubmission,
      onRejected: (conflict?: Problem) => void,
    ) => void;
    resetSubmission: () => void;
    workflow: Workflow;
    query: URLSearchParams;
    reloadWorkflow: (problem: Problem, restored?: URLSearchParams) => void;
  } = $props();
  const restored = untrack(() => restoreFormState(workflow, query));
  const uid = $props.id();
  const fieldId = (name: string) => `${uid}-${encodeURIComponent(name)}`;
  const initiallyAdvanced = untrack(() =>
    [...restored.fields].some(
      ([name, field]) =>
        workflow.parameters[name].advanced &&
        (field.seedMode === 'random' ||
          field.source === 'user' ||
          field.source === 'preset' ||
          restored.warnings.some(({ name: warned }) => warned === name) ||
          restored.differences.some(({ name: changed }) => changed === name)),
    ),
  );
  const fields = new SvelteMap(restored.fields);
  const chosenSeeds = new SvelteMap(restored.chosenSeeds);
  let pendingSuggestion = $state.raw<PendingSuggestion | undefined>(
    restored.pendingSuggestion,
  );
  let suggestionErrors = $state.raw<string[]>([]);
  let suggestionStatus = $state('');
  let warnings = $state.raw(restored.warnings);
  let differences = $state.raw(restored.differences);
  let shareMessage = $state('');
  let shareFallback = $state('');
  const uploadErrors = new SvelteMap<string, string>();
  const imageEpochs = new SvelteMap<string, number>();
  let basic = $derived(parameterGroups(workflow, false));
  let advanced = $derived(parameterGroups(workflow, true));
  let advancedOpen = $state(initiallyAdvanced);
  let preset = $state(restored.preset);
  let modified = $state(restored.modified);
  let showErrors = $state(false);
  let reviewErrors = $state.raw<Record<string, string>>({});
  let review = $state.raw<Snapshot>();
  let validationProblem = $state<Problem>();
  let validating = $state(false);
  let validation: AbortController | undefined;
  onDestroy(() => validation?.abort());
  let reviewHeading = $state<HTMLElement>();
  let limit = $state<number>();

  $effect(() => {
    if (
      !Object.values(workflow.parameters).some(
        ({ kind }) => kind === 'image_upload',
      )
    )
      return;
    const controller = new AbortController();
    void readLimit(controller.signal);
    return () => controller.abort();
  });

  async function readLimit(signal: AbortSignal) {
    try {
      const { settings } = await api.getSettings({ signal });
      const setting = settings['server.max_upload_bytes'];
      const value = setting?.active_value ?? setting?.value;
      if (
        !signal.aborted &&
        typeof value === 'number' &&
        Number.isSafeInteger(value) &&
        value > 0
      )
        limit = value;
    } catch {
      // The upload endpoint still enforces the limit and returns problem details.
    }
  }

  function invalidateReview() {
    validation?.abort();
    validation = undefined;
    validating = false;
    review = undefined;
    reviewErrors = {};
    validationProblem = undefined;
    resetSubmission();
    shareFallback = '';
    shareMessage = '';
  }

  function problem(failure: unknown, title: string): Problem {
    return failure instanceof ApiProblemError
      ? failure.problem
      : {
          title,
          detail: failure instanceof Error ? failure.message : undefined,
        };
  }

  function pendingValue(field: FieldState) {
    return field.draft !== undefined ? field.draft : displayScalar(field.value);
  }

  function pendingDisplayRows(pending: PendingSuggestion) {
    const rows: { name: string; value: string }[] = [];
    const presetValues = workflow.presets[pending.preset];
    for (const [name, raw] of Object.entries(presetValues ?? {})) {
      rows.push({
        name: `${name} (preset)`,
        value: displayScalar(raw as FieldState['value']),
      });
    }
    for (const [name, field] of pending.fields)
      rows.push({ name, value: pendingValue(field) });
    return rows;
  }

  function applySuggestion() {
    const pending = pendingSuggestion;
    if (!pending || editingLocked) return;
    const errors = [...pending.issues];
    const presetValues = pending.preset
      ? workflow.presets[pending.preset]
      : undefined;
    if (pending.preset && !presetValues)
      errors.push(
        `Preset “${pending.preset}” is not available in revision ${workflow.revision}.`,
      );
    const prepared = new SvelteMap<string, FieldState>();
    for (const [name, raw] of Object.entries(presetValues ?? {})) {
      const parameter = workflow.parameters[name];
      if (!parameter) {
        errors.push(
          `Preset “${pending.preset}” refers to unknown parameter “${name}”.`,
        );
        continue;
      }
      const field = presetField(parameter, initialField(parameter), raw);
      const error = bindingError(parameter, field);
      if (error) errors.push(`Parameter “${name}” in the preset: ${error}`);
      prepared.set(name, field);
    }
    for (const [name, field] of pending.fields) {
      const parameter = workflow.parameters[name];
      if (!parameter) {
        errors.push(
          `Parameter “${name}” is not available in revision ${workflow.revision}.`,
        );
        continue;
      }
      const error = bindingError(parameter, field);
      if (error)
        errors.push(
          `Parameter “${name}” with suggested value ${pendingValue(field)}: ${error}`,
        );
      else prepared.set(name, field);
    }
    if (errors.length) {
      suggestionErrors = errors;
      return;
    }
    preset = pending.preset;
    modified = pending.modified;
    for (const [name, field] of prepared) {
      fields.set(name, field);
      chosenSeeds.delete(name);
      resetUpload(name);
      if (workflow.parameters[name].advanced) advancedOpen = true;
    }
    pendingSuggestion = undefined;
    suggestionErrors = [];
    suggestionStatus = `Applied the assistant suggestion to loaded revision ${workflow.revision}. Review every value before submitting.`;
    invalidateReview();
  }

  function discardSuggestion() {
    if (editingLocked) return;
    pendingSuggestion = undefined;
    suggestionErrors = [];
    suggestionStatus =
      'The assistant suggestion was discarded. The current form values were kept.';
    invalidateReview();
  }

  function discardWarnings() {
    for (const warning of warnings) {
      if (warning.name === undefined) continue;
      fields.set(warning.name, initialField(workflow.parameters[warning.name]));
      differences = differences.filter(({ name }) => name !== warning.name);
      chosenSeeds.delete(warning.name);
      if (preset) modified = true;
    }
    if (preset && !Object.hasOwn(workflow.presets, preset)) preset = '';
    warnings = [];
    invalidateReview();
  }

  function shareHash() {
    const chosen = Object.fromEntries(chosenSeeds);
    for (const row of review?.rows ?? []) {
      if (row.random && row.value !== undefined)
        Object.defineProperty(chosen, row.name, {
          value: row.value,
          enumerable: true,
        });
    }
    return encodeFormState({
      workflow: workflow.id,
      version: workflow.version,
      revision: workflow.revision,
      unverifiedSource: restored.unverifiedSource,
      suggested: restored.suggested,
      preset,
      modified,
      fields,
      chosenSeeds: chosen,
      inherited: review ? [...review.inherited.values()] : restored.inherited,
    });
  }

  function workflowChanged(failure: unknown): failure is ApiProblemError {
    return (
      failure instanceof ApiProblemError &&
      failure.status === 409 &&
      failure.type === 'urn:genaimage:problem:workflow-changed'
    );
  }

  function reloadChangedWorkflow(conflict: Problem) {
    let restored: URLSearchParams | undefined;
    try {
      restored = new URLSearchParams(shareHash().slice('#/generate?'.length));
    } catch {
      conflict = {
        ...conflict,
        detail:
          `${conflict.detail ?? ''} The form was too large to restore; enter the values again after reloading.`.trim(),
      };
    }
    invalidateReview();
    reloadWorkflow(conflict, restored);
  }

  async function copyShareLink() {
    let hash: string;
    try {
      hash = shareHash();
    } catch (failure) {
      shareFallback = '';
      shareMessage =
        failure instanceof Error
          ? failure.message
          : 'Could not create the share link.';
      return;
    }
    const url = new URL(hash, location.href).href;
    try {
      await navigator.clipboard.writeText(url);
      shareMessage = 'Share link copied.';
      shareFallback = '';
    } catch {
      shareFallback = url;
      shareMessage = 'Copy the share link below.';
    }
  }

  function applyPreset(name: string | undefined) {
    if (editingLocked || !name || !Object.hasOwn(workflow.presets, name))
      return;
    for (const [key, field] of fields) {
      if (Object.hasOwn(workflow.presets[name], key)) {
        fields.set(
          key,
          presetField(
            workflow.parameters[key],
            field,
            workflow.presets[name][key],
          ),
        );
        if (fields.get(key)?.highlighted && workflow.parameters[key].advanced)
          advancedOpen = true;
        chosenSeeds.delete(key);
        warnings = warnings.filter((warning) => warning.name !== key);
        differences = differences.filter(
          (difference) => difference.name !== key,
        );
        resetUpload(key);
      } else if (field.highlighted) {
        fields.set(key, { ...field, highlighted: false });
      }
    }
    preset = name;
    modified = false;
    invalidateReview();
  }

  function edit(name: string, next: FieldState) {
    if (editingLocked) return;
    fields.set(name, next);
    chosenSeeds.delete(name);
    warnings = warnings.filter((warning) => warning.name !== name);
    differences = differences.filter((difference) => difference.name !== name);
    if (preset) modified = true;
    invalidateReview();
  }

  function resetUpload(name: string) {
    if (workflow.parameters[name].kind !== 'image_upload') return;
    uploadErrors.delete(name);
    imageEpochs.set(name, (imageEpochs.get(name) ?? 0) + 1);
  }

  function uploadStatus(name: string, message?: string) {
    if (message) uploadErrors.set(name, message);
    else uploadErrors.delete(name);
    invalidateReview();
  }

  function positivePromptFor(name: string) {
    for (const [candidate, parameter] of Object.entries(workflow.parameters)) {
      if (
        candidate === name ||
        !isPromptParameter(candidate, parameter) ||
        isNegativePromptParameter(candidate, parameter)
      )
        continue;
      const field = fields.get(candidate);
      if (typeof field?.value === 'string') return field.value;
    }
    const own = fields.get(name);
    return typeof own?.value === 'string' ? own.value : '';
  }

  function fieldError(name: string, field: FieldState) {
    if (uploadErrors.has(name)) return uploadErrors.get(name);
    if (field.seedMode === 'random') return undefined;
    const parameter = workflow.parameters[name];
    if (
      field.value === undefined &&
      field.draft === undefined &&
      initialField(parameter).source
    )
      return 'Enter a value or reset to the manifest default.';
    return bindingError(parameter, field);
  }

  async function requestReview(event: SubmitEvent) {
    event.preventDefault();
    if (editingLocked || validating || pendingSuggestion || warnings.length)
      return;
    showErrors = true;
    invalidateReview();
    const entries = [...fields];
    const errors = new SvelteMap<string, string>();
    const values = new SvelteMap<string, NonNullable<FieldState['value']>>();
    for (const [name, field] of entries) {
      const error = fieldError(name, field);
      if (error) errors.set(name, error);
      else if (field.seedMode !== 'random' && field.value !== undefined)
        values.set(name, field.value);
    }
    if (!errors.size) {
      for (const [name, field] of entries) {
        if (field.seedMode !== 'random') continue;
        const value =
          chosenSeeds.get(name) ?? randomSeed(workflow.parameters[name]);
        if (value === undefined)
          errors.set(name, 'This workflow has no valid seed to randomise.');
        else values.set(name, value);
      }
      for (const [name, value] of values) {
        if (
          (typeof value === 'bigint' || value instanceof BindingFloat) &&
          !supportsExactNumberWrites()
        )
          errors.set(
            name,
            'Update your browser to send this number without changing it.',
          );
      }
    }
    if (errors.size) {
      await showFieldErrors(errors);
      return;
    }
    const parameters = Object.freeze(Object.fromEntries(values));
    // Do not ask the API to apply the preset again: it could supply unreviewed values.
    const revision = workflow.revision;
    if (!revision) {
      validationProblem = {
        title: 'The server did not provide a workflow revision',
        recovery: 'Reload the workflow before reviewing it.',
      };
      return;
    }
    const bindings = { parameters, workflow_revision: revision };
    const body = Object.freeze({ workflow: workflow.id, ...bindings });
    const rows = entries.map(([name, field]) => ({
      name,
      value: values.get(name),
      source: field.seedMode === 'random' ? ('user' as const) : field.source,
      random: field.seedMode === 'random',
      image: workflow.parameters[name].kind === 'image_upload',
      requirement: requirementLabel(workflow.parameters[name]),
      sharedDefault:
        field.source === 'default' &&
        differences.some((difference) => difference.name === name),
    }));
    const controller = new AbortController();
    validation = controller;
    validating = true;
    try {
      const result = await api.validateWorkflow(workflow.id, bindings, {
        exactNumbers: true,
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      if (result.revision !== revision) {
        reloadChangedWorkflow({
          title: 'The workflow changed during validation',
          recovery: 'Review the reloaded workflow before submitting.',
        });
        return;
      }
      const problems = new SvelteMap<string, string>();
      const otherErrors: { key: string; reason: string }[] = [];
      for (const { parameter, reason } of result.parameter_errors) {
        if (fields.has(parameter)) problems.set(parameter, reason);
        else otherErrors.push({ key: parameter, reason });
      }
      if (
        !result.valid ||
        problems.size ||
        otherErrors.length ||
        result.node_check.status !== 'available'
      ) {
        const node = result.node_check;
        validationProblem = {
          title: 'The workflow did not pass server validation',
          detail:
            node.status === 'missing'
              ? `Missing backend nodes: ${node.missing_classes.join(', ')}.`
              : (node.reason ?? undefined),
          recovery: 'Correct the errors and review the parameters again.',
          errors: otherErrors,
        };
        await showFieldErrors(problems);
        return;
      }
      const inherited = new SvelteMap<string, SharedInherited>();
      for (const {
        parameter,
        node,
        input,
        source,
      } of result.inherited_inputs ?? []) {
        if (!fields.has(parameter) || Object.hasOwn(parameters, parameter))
          continue;
        const description =
          source.kind === 'connection'
            ? `connected to node ${source.node} output ${source.output}`
            : source.json;
        inherited.set(parameter, [parameter, node, input, description]);
      }
      if (
        rows.some(
          ({ name, value }) => value === undefined && !inherited.has(name),
        )
      ) {
        validationProblem = {
          title: 'The server did not report all inherited workflow inputs',
          recovery: 'Update the local server and review the workflow again.',
        };
        return;
      }
      const inheritedChanges = restored.inherited.flatMap((previous) => {
        const [name, node, input, description] = previous;
        const current = inherited.get(name);
        if (current && previous.every((part, index) => part === current[index]))
          return [];
        return [
          `${name}: The saved link inherited ${description} (node ${node}, input ${input}). ${current ? `The current workflow inherits ${current[3]} (node ${current[1]}, input ${current[2]}).` : 'This input is no longer inherited.'}`,
        ];
      });
      review = {
        body,
        rows,
        inherited,
        inheritedChanges,
        outputs: workflow.outputs.map(({ node, kind, declared_kind }) => ({
          node,
          kind: `${kind}${presenceLabel(declared_kind)}`,
        })),
        workflow: workflow.id,
        version: workflow.version,
        preset,
        modified,
        key: crypto.randomUUID(),
      };
      await tick();
      reviewHeading?.focus();
    } catch (failure) {
      if (controller.signal.aborted) return;
      if (workflowChanged(failure)) reloadChangedWorkflow(failure.problem);
      else
        validationProblem = problem(failure, 'Could not validate the workflow');
    } finally {
      if (validation === controller) {
        validating = false;
        validation = undefined;
      }
    }
  }

  async function showFieldErrors(errors: ReadonlyMap<string, string>) {
    reviewErrors = Object.fromEntries(errors);
    if ([...errors.keys()].some((name) => workflow.parameters[name].advanced))
      advancedOpen = true;
    const first = [...fields.keys()].find((name) => errors.has(name));
    if (first !== undefined) {
      await tick();
      document.getElementById(fieldId(first))?.focus();
    }
  }

  function submitReview() {
    if (!review || pendingSuggestion || editingLocked || accepted) return;
    submit(review, (conflict) => {
      if (conflict) reloadChangedWorkflow(conflict);
      else review = undefined;
    });
  }
</script>

<h2>{workflow.title}</h2>
{#if workflow.description}<p>{workflow.description}</p>{/if}
{#if pendingSuggestion}
  <section
    class="suggestion-mismatch"
    role="alert"
    aria-labelledby={`${uid}-suggestion-mismatch`}
  >
    <h3 id={`${uid}-suggestion-mismatch`}>Assistant suggestion needs review</h3>
    <p>
      The suggestion was created for revision
      <code>{pendingSuggestion.suggestedRevision}</code>, but this page loaded
      revision <code>{pendingSuggestion.loadedRevision}</code>. No suggested
      value has been applied. Review and submit are blocked until you choose an
      action.
    </p>
    <h4>Pending values</h4>
    <dl>
      {#if pendingSuggestion.preset}
        <dt>Preset</dt>
        <dd><code>{JSON.stringify(pendingSuggestion.preset)}</code></dd>
      {/if}
      {#each pendingDisplayRows(pendingSuggestion) as row (`${row.name}:${row.value}`)}
        <dt>{row.name}</dt>
        <dd><code>{row.value}</code></dd>
      {/each}
    </dl>
    {#if pendingSuggestion.issues.length}<ProblemDetails
        problem={{
          title: 'The suggestion cannot be restored exactly',
          recovery: 'Discard it or request a new suggestion for this revision.',
          errors: pendingSuggestion.issues.map((reason) => ({
            key: 'Suggestion',
            reason,
          })),
        }}
      />{/if}
    {#if suggestionErrors.length}<ProblemDetails
        problem={{
          title: 'The suggestion is invalid for the loaded revision',
          recovery:
            'Correct the source suggestion or discard these pending values.',
          errors: suggestionErrors.map((reason) => ({
            key: 'Suggestion',
            reason,
          })),
        }}
      />{/if}
    <div class="suggestion-actions">
      <Button
        variant="primary"
        onclick={applySuggestion}
        disabled={editingLocked}>Apply to current revision</Button
      >
      <Button onclick={discardSuggestion} disabled={editingLocked}
        >Discard</Button
      >
    </div>
  </section>
{/if}
{#if suggestionStatus}<p role="status">{suggestionStatus}</p>{/if}
{#if restored.versionNotice}<p role="status">{restored.versionNotice}</p>{/if}
{#if restored.suggestionNotice && !pendingSuggestion && !suggestionStatus}<p
    role="status"
    class="source"
  >
    {restored.suggestionNotice}
  </p>{/if}
{#if restored.revisionNotice}<p role="status">{restored.revisionNotice}</p>{/if}
{#if restored.unverifiedSource}<p role="status">
    Remix uses the current workflow; it cannot be verified against the source
    run.
  </p>{/if}
{#if warnings.length}
  <ProblemDetails
    problem={{
      title: 'Some shared values need attention',
      recovery:
        'Correct the fields or explicitly discard the unresolved shared values.',
      errors: warnings.map(({ name, message }) => ({
        key: name ?? 'Share link',
        reason: message,
      })),
    }}
  />
  <Button onclick={discardWarnings} disabled={editingLocked}
    >Discard unresolved shared values</Button
  >
{/if}
<form novalidate onsubmit={requestReview}>
  <fieldset class="editor" disabled={editingLocked}>
    {#if preset || Object.keys(workflow.presets).length}
      <div class="presets">
        <Select
          label="Preset"
          hint="Choose a preset to fill its parameters. Other fields stay as they are."
          options={[
            { value: '', label: 'Choose a preset', disabled: true },
            ...(preset && !Object.hasOwn(workflow.presets, preset)
              ? [
                  {
                    value: preset,
                    label: `${preset} (unavailable)`,
                    disabled: true,
                  },
                ]
              : []),
            ...Object.keys(workflow.presets).map((name) => ({
              value: name,
              label: name,
            })),
          ]}
          bind:value={() => preset, applyPreset}
        />
        {#if preset}<p role="status">
            {preset}
            {modified ? '(modified)' : 'applied'}
          </p>{/if}
      </div>
    {/if}
    {#snippet groups(items: ReturnType<typeof parameterGroups>)}
      {#each items as [group, parameters] (group)}
        <fieldset>
          <legend>{group || 'Parameters'}</legend>
          {#each parameters as [name, parameter] (name)}
            {@const field = fields.get(name) ?? initialField(parameter)}
            {#if chosenSeeds.has(name)}
              <p>
                Shared seed for {name}:
                <code>{String(chosenSeeds.get(name))}</code>
              </p>
              <Button
                onclick={() => {
                  chosenSeeds.delete(name);
                  if (preset) modified = true;
                  invalidateReview();
                }}>Choose a new seed on review</Button
              >
            {/if}
            {@const difference = differences.find((item) => item.name === name)}
            {#if difference}<p class="source">
                {name}: {difference.message}
              </p>{/if}
            <ParameterField
              sharedDefault={!!difference && field.source === 'default'}
              id={fieldId(name)}
              {name}
              {parameter}
              {field}
              {limit}
              imageEpoch={imageEpochs.get(name)}
              error={uploadErrors.get(name) ??
                (showErrors
                  ? Object.hasOwn(reviewErrors, name)
                    ? reviewErrors[name]
                    : fieldError(name, field)
                  : undefined)}
              update={(next) => edit(name, next)}
              reset={() => {
                resetUpload(name);
                edit(name, initialField(parameter));
              }}
              uploadStatus={(message) => uploadStatus(name, message)}
              workflowId={workflow.id}
              positivePrompt={positivePromptFor(name)}
            />
          {/each}
        </fieldset>
      {/each}
    {/snippet}
    {#if basic.length}
      <section aria-label="Basic parameters">
        <h3>Basic parameters</h3>
        {@render groups(basic)}
      </section>
    {/if}
    {#if advanced.length}
      <details bind:open={advancedOpen}>
        <summary>Advanced parameters</summary>
        {@render groups(advanced)}
      </details>
    {/if}
    <Button
      type="submit"
      variant="primary"
      disabled={validating || !!pendingSuggestion || warnings.length > 0}
      >Review parameters</Button
    >
  </fieldset>
</form>
<div class="share">
  <Button
    onclick={copyShareLink}
    disabled={!!pendingSuggestion || warnings.length > 0}
    >Copy share link</Button
  >
  {#if shareMessage}<p role="status">{shareMessage}</p>{/if}
  {#if shareFallback}<Input
      label="Share link"
      value={shareFallback}
      readonly
    />{/if}
</div>
{#if validating}<p role="status">Validating the workflow…</p>{/if}
{#if validationProblem}<ProblemDetails problem={validationProblem} />{/if}
{#if review && !editingLocked}
  <section class="review" aria-labelledby={`${uid}-review`}>
    <h3 id={`${uid}-review`} tabindex="-1" bind:this={reviewHeading}>
      Review parameters
    </h3>
    <p>Workflow: <code>{review.workflow}</code> · Version: {review.version}</p>
    <p>Workflow revision: <code>{review.body.workflow_revision}</code></p>
    <p>
      Preset: {review.preset || 'None'}{review.modified && review.preset
        ? ' (modified)'
        : ''}
    </p>
    {#if review.inheritedChanges.length}
      <div role="status">
        <p>Inherited inputs differ from the saved link.</p>
        <ul>
          {#each review.inheritedChanges as change, index (index)}<li>
              {change}
            </li>{/each}
        </ul>
      </div>
    {/if}
    {#if review.rows.length}
      <dl>
        {#each review.rows as { name, value, source, random, image, requirement, sharedDefault } (name)}
          {@const inherited = review.inherited.get(name)}
          <dt>
            {name}{random ? ' (randomised seed)' : ''}
            <span class="source">· {requirement}</span>
          </dt>
          <dd>
            {#if value === undefined && inherited}
              Inherited from workflow: {inherited[3]}
              <span class="source"
                >(node {inherited[1]}, input {inherited[2]})</span
              >
            {:else if image && typeof value === 'string'}<code>{value}</code>
            {:else}{value === '' ? '(empty text)' : String(value)}{/if}
            {#if source && value !== undefined}
              <span class="source"
                >({sharedDefault ? 'shared default' : source})</span
              >{/if}
            {#if image && typeof value === 'string'}<br />SHA-256:
              <code>{value.slice('sha256:'.length)}</code>{/if}
          </dd>
        {/each}
      </dl>
    {:else}<p>No parameters are set.</p>{/if}
    {#if review.outputs.length}
      <h4>Expected outputs</h4>
      <ul>
        {#each review.outputs as { node, kind }, index (index)}
          <li>Node <code>{node}</code> · {kind}</li>
        {/each}
      </ul>
    {/if}
    {#if !accepted && !editingLocked}
      <Button variant="primary" onclick={submitReview}>Submit run</Button>
    {/if}
  </section>
{/if}

<style>
  .editor {
    display: grid;
    border: 0;
    margin: 0;
    padding: 0;
    gap: 1rem;
    justify-items: start;
  }
  form,
  .share,
  .review {
    max-width: 50rem;
  }
  section,
  details,
  .presets {
    width: 100%;
    min-width: 0;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    margin: 0.5rem 0 0;
    padding: 0.4rem;
    min-width: 0;
  }
  summary {
    cursor: pointer;
    font-weight: 600;
  }
  dd {
    margin: 0 0 0.6rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .share {
    margin-block: 1rem;
  }
  .source {
    color: var(--muted);
  }
  .suggestion-mismatch {
    border: 0.15rem solid var(--danger);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
    margin-block: var(--space-3);
    max-width: 50rem;
  }
  .suggestion-mismatch dl {
    display: grid;
    grid-template-columns: minmax(7rem, max-content) 1fr;
    gap: var(--space-1) var(--space-3);
  }
  .suggestion-actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  dt {
    font-weight: 600;
  }
</style>
