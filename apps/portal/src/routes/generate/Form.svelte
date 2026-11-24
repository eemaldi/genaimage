<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import {
    api,
    ApiHttpError,
    ApiProblemError,
    type SubmitRunRequest,
    BindingFloat,
    supportsExactNumberWrites,
  } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { components } from '../../lib/api/schema';
  import RunProgress from './RunProgress.svelte';
  import { encodeFormState, restoreFormState } from './share';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ParameterField from './ParameterField.svelte';
  import {
    bindingError,
    initialField,
    parameterGroups,
    presetField,
    randomSeed,
    type FieldState,
    type Review,
    type Workflow,
  } from './model';

  type Run = components['schemas']['RunResponse'];
  type Snapshot = {
    body: Readonly<SubmitRunRequest>;
    rows: ReadonlyArray<{
      name: string;
      value?: Review[string];
      source?: FieldState['source'];
      random: boolean;
      image: boolean;
    }>;
    workflow: string;
    version: string;
    preset: string;
    modified: boolean;
    key: string;
  };
  let { workflow, query }: { workflow: Workflow; query: URLSearchParams } =
    $props();
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
          restored.warnings.some(({ name: warned }) => warned === name)),
    ),
  );
  const fields = new SvelteMap(restored.fields);
  const chosenSeeds = new SvelteMap(restored.chosenSeeds);
  let warnings = $state.raw(restored.warnings);
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
  let submissionProblem = $state<Problem>();
  let validating = $state(false);
  let submitting = $state(false);
  let ambiguous = $state(false);
  let knownRunId = $state<string>();
  let accepted = $state(false);
  let run = $state.raw<Run>();
  let editingLocked = $derived(submitting || ambiguous);
  let validation: AbortController | undefined;
  let submission: AbortController | undefined;
  onDestroy(() => {
    validation?.abort();
    submission?.abort();
  });
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
    submissionProblem = undefined;
    accepted = false;
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

  function discardWarnings() {
    for (const warning of warnings) {
      if (warning.name === undefined) continue;
      fields.set(warning.name, initialField(workflow.parameters[warning.name]));
      chosenSeeds.delete(warning.name);
      if (preset) modified = true;
    }
    if (preset && !Object.hasOwn(workflow.presets, preset)) preset = '';
    warnings = [];
    invalidateReview();
  }

  async function copyShareLink() {
    const chosen = Object.fromEntries(chosenSeeds);
    for (const row of review?.rows ?? []) {
      if (row.random && row.value !== undefined)
        Object.defineProperty(chosen, row.name, {
          value: row.value,
          enumerable: true,
        });
    }
    const hash = encodeFormState({
      workflow: workflow.id,
      version: workflow.version,
      preset,
      modified,
      fields,
      chosenSeeds: chosen,
    });
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
    if (editingLocked || validating || warnings.length) return;
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
    const body = Object.freeze({ workflow: workflow.id, parameters });
    const rows = entries.map(([name, field]) => ({
      name,
      value: values.get(name),
      source: field.seedMode === 'random' ? ('user' as const) : field.source,
      random: field.seedMode === 'random',
      image: workflow.parameters[name].kind === 'image_upload',
    }));
    const controller = new AbortController();
    validation = controller;
    validating = true;
    try {
      const result = await api.validateWorkflow(
        workflow.id,
        { parameters },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
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
      review = {
        body,
        rows,
        workflow: workflow.id,
        version: workflow.version,
        preset,
        modified,
        key: crypto.randomUUID(),
      };
      await tick();
      reviewHeading?.focus();
    } catch (failure) {
      if (!controller.signal.aborted)
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

  async function submitReview() {
    const snapshot = review;
    if (!snapshot || submitting || accepted) return;
    submitting = true;
    submissionProblem = undefined;
    const controller = new AbortController();
    submission = controller;
    try {
      const result = await api.createRun(snapshot.body, snapshot.key, {
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      run = result;
      ambiguous = false;
      knownRunId = undefined;
      accepted = true;
    } catch (failure) {
      if (controller.signal.aborted) return;
      const status =
        failure instanceof ApiProblemError || failure instanceof ApiHttpError
          ? failure.status
          : undefined;
      ambiguous =
        status === undefined || status === 408 || status < 400 || status >= 500;
      submissionProblem = problem(
        failure,
        ambiguous
          ? 'The submission outcome is uncertain'
          : 'Could not submit the run',
      );
      if (ambiguous && failure instanceof ApiProblemError) {
        const { run_id: id, instance } = failure.problem;
        const pathId = instance?.match(
          /^\/api\/v1\/runs\/([a-zA-Z0-9_-]+)$/,
        )?.[1];
        if (typeof id === 'string' && id) knownRunId = id;
        else if (pathId) knownRunId = pathId;
      }
      if (!ambiguous) {
        review = undefined;
        knownRunId = undefined;
      }
    } finally {
      if (submission === controller) {
        submitting = false;
        submission = undefined;
      }
    }
  }
</script>

<h2>{workflow.title}</h2>
{#if workflow.description}<p>{workflow.description}</p>{/if}
{#if restored.versionNotice}<p role="status">{restored.versionNotice}</p>{/if}
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
  <Button onclick={discardWarnings}>Discard unresolved shared values</Button>
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
            <ParameterField
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
      disabled={validating || warnings.length > 0}>Review parameters</Button
    >
  </fieldset>
</form>
<div class="share">
  <Button onclick={copyShareLink} disabled={warnings.length > 0}
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
{#if submissionProblem}<ProblemDetails problem={submissionProblem} />{/if}
{#if review}
  <section class="review" aria-labelledby={`${uid}-review`}>
    <h3 id={`${uid}-review`} tabindex="-1" bind:this={reviewHeading}>
      Review parameters
    </h3>
    <p>Workflow: <code>{review.workflow}</code> · Version: {review.version}</p>
    <p>
      Preset: {review.preset || 'None'}{review.modified && review.preset
        ? ' (modified)'
        : ''}
    </p>
    {#if review.rows.length}
      <dl>
        {#each review.rows as { name, value, source, random, image } (name)}
          <dt>{name}{random ? ' (randomised seed)' : ''}</dt>
          <dd>
            {#if image && typeof value === 'string'}<code>{value}</code>
            {:else}{value === undefined
                ? 'Not set — not submitted'
                : value === ''
                  ? '(empty text)'
                  : String(value)}{/if}
            {#if source && value !== undefined}
              <span class="source">({source})</span>{/if}
            {#if image && typeof value === 'string'}<br />SHA-256:
              <code>{value.slice('sha256:'.length)}</code>{/if}
          </dd>
        {/each}
      </dl>
    {:else}<p>No parameters are set.</p>{/if}
    {#if ambiguous}
      <p>
        The run may already exist. {#if knownRunId}<a
            href={`#/runs/${encodeURIComponent(knownRunId)}`}
            >Check the run details</a
          > or
        {/if}<a href="#/jobs">check Jobs</a> to reconcile it. Retrying here sends
        the identical request with the same key.
      </p>
      <Button onclick={submitReview} disabled={submitting}
        >Retry with the same key</Button
      >
    {:else if !accepted}
      <Button variant="primary" onclick={submitReview} disabled={submitting}
        >Submit run</Button
      >
    {/if}
    {#if submitting}<p role="status">Submitting the reviewed run…</p>{/if}
  </section>
{/if}
{#if run}{#key run.id}<RunProgress initial={run} />{/key}{/if}

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
  dt {
    font-weight: 600;
  }
</style>
