<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import {
    api,
    ApiHttpError,
    ApiProblemError,
    supportsExactNumberWrites,
    type SubmitBatchRequest,
  } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import { navigateHash } from '../../navigation';
  import ParameterField from '../generate/ParameterField.svelte';
  import { restoreFormState } from '../generate/share';
  import {
    bindingError,
    initialField,
    parameterGroups,
    presetField,
    randomSeed,
    type FieldState,
    type Scalar,
    type Workflow,
  } from '../generate/model';
  import {
    clearPendingBatch,
    loadPendingBatch,
    savePendingBatch,
    type PendingBatch,
  } from './pending';
  import BatchPreview, { type Review } from './BatchPreview.svelte';
  import {
    expectedCount,
    maxItemsCap,
    maxRandomSeeds,
    parseValues,
    randomSeeds,
    requiresExactWrites,
    showValue,
    type ParsedValues,
  } from './sweep';

  let {
    workflow,
    query,
    reloadWorkflow,
  }: {
    workflow: Workflow;
    query: URLSearchParams;
    reloadWorkflow: (problem: Problem) => void;
  } = $props();
  const uid = $props.id();
  const restored = untrack(() => restoreFormState(workflow, query));
  const recovered = untrack(() => {
    try {
      return { pending: loadPendingBatch(workflow.id) };
    } catch (failure) {
      return {
        problem: problemOf(failure, 'Could not restore a pending batch'),
      };
    }
  });

  const fields = new SvelteMap(restored.fields);
  const uploadErrors = new SvelteMap<string, string>();
  const parameters = untrack(() => workflow.parameters);
  const seedNames = Object.entries(parameters)
    .filter(([, parameter]) => parameter.kind === 'seed')
    .map(([name]) => name);
  const gridNames = Object.entries(parameters)
    .filter(([, parameter]) => parameter.kind !== 'image_upload')
    .map(([name]) => name);
  const chosen = new SvelteSet<string>();
  const gridTexts = new SvelteMap<string, string>();

  let preset = $state(restored.preset);
  let sweepKind = $state<'seeds' | 'grid'>(seedNames.length ? 'seeds' : 'grid');
  let seedName = $state(seedNames[0] ?? '');
  let seedText = $state('');
  let randomCount = $state('4');
  let seedStatus = $state('');
  let maxItemsText = $state<string>();
  let showErrors = $state(false);
  let review = $state.raw<Review>();
  let pending = $state.raw<PendingBatch | undefined>(recovered.pending);
  let problem = $state<Problem | undefined>(recovered.problem);
  let formErrors = $state.raw<string[]>([]);
  let working = $state(false);
  let controller: AbortController | undefined;
  let problemHeading = $state<HTMLElement>();
  let reviewHeading = $state<HTMLElement>();
  onDestroy(() => controller?.abort());

  let locked = $derived(working || !!pending || !!recovered.problem);
  let swept = $derived(
    sweepKind === 'seeds' ? (seedName ? [seedName] : []) : [...chosen].sort(),
  );
  let parsed = $derived.by(() => {
    const result = new SvelteMap<string, ParsedValues>();
    for (const name of swept) {
      const text =
        sweepKind === 'seeds' ? seedText : (gridTexts.get(name) ?? '');
      result.set(name, parseValues(workflow.parameters[name], text));
    }
    return result;
  });
  let count = $derived(swept.length ? expectedCount([...parsed.values()]) : 0);
  let maxItems = $derived(maxItemsText ?? String(count));

  function problemOf(failure: unknown, title: string): Problem {
    return failure instanceof ApiProblemError
      ? failure.problem
      : {
          title,
          detail: failure instanceof Error ? failure.message : undefined,
        };
  }

  function invalidate() {
    controller?.abort();
    controller = undefined;
    working = false;
    review = undefined;
    problem = undefined;
    formErrors = [];
  }

  function edit(name: string, next: FieldState) {
    if (locked) return;
    fields.set(name, next);
    invalidate();
  }

  function applyPreset(name: string | undefined) {
    if (locked || !name || !Object.hasOwn(workflow.presets, name)) return;
    for (const [key, field] of fields) {
      if (!Object.hasOwn(workflow.presets[name], key)) continue;
      fields.set(
        key,
        presetField(
          workflow.parameters[key],
          field,
          workflow.presets[name][key],
        ),
      );
    }
    preset = name;
    invalidate();
  }

  function addRandomSeeds() {
    const wanted = Number(randomCount);
    if (
      locked ||
      !seedName ||
      !Number.isInteger(wanted) ||
      wanted < 1 ||
      wanted > maxRandomSeeds
    ) {
      seedStatus = `Enter a whole number from 1 to ${maxRandomSeeds}.`;
      return;
    }
    const added = randomSeeds(
      workflow.parameters[seedName],
      wanted,
      randomSeed,
      parsed.get(seedName)?.values ?? [],
    );
    if (!added.length) {
      seedStatus = 'No further valid seeds could be generated.';
      return;
    }
    seedText = [seedText.replace(/\s+$/, ''), ...added.map(String)]
      .filter(Boolean)
      .join('\n');
    seedStatus = `Added ${added.length} random ${added.length === 1 ? 'seed' : 'seeds'} to the list. They are ordinary list entries you can edit.`;
    invalidate();
  }

  function toggleGrid(name: string, on: boolean) {
    if (locked) return;
    if (on) chosen.add(name);
    else chosen.delete(name);
    invalidate();
  }

  function baseError(name: string, field: FieldState) {
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

  function describeMaxItems(): { value?: number; error?: string } {
    if (!/^\d+$/.test(maxItems.trim()))
      return { error: 'Enter the most runs you accept, as a whole number.' };
    const value = Number(maxItems);
    if (value < 1 || value > maxItemsCap)
      return { error: `Enter a number from 1 to ${maxItemsCap}.` };
    return { value };
  }

  async function requestReview(event: SubmitEvent) {
    event.preventDefault();
    if (locked) return;
    showErrors = true;
    invalidate();
    const errors: string[] = [];
    if (!swept.length)
      errors.push(
        sweepKind === 'seeds'
          ? 'This workflow has no seed parameter to sweep.'
          : 'Choose at least one parameter for the grid.',
      );
    for (const name of swept)
      for (const error of parsed.get(name)?.errors ?? [])
        errors.push(`${name}: ${error}`);
    const limit = describeMaxItems();
    if (limit.error) errors.push(`Maximum runs: ${limit.error}`);
    const values = new SvelteMap<string, Scalar>();
    const random = new SvelteSet<string>();
    for (const [name, field] of fields) {
      if (swept.includes(name)) continue;
      const error = baseError(name, field);
      if (error) errors.push(`${name}: ${error}`);
      else if (field.seedMode === 'random') {
        const seed = randomSeed(workflow.parameters[name]);
        if (seed === undefined)
          errors.push(`${name}: This workflow has no valid seed to randomise.`);
        else {
          values.set(name, seed);
          random.add(name);
        }
      } else if (field.value !== undefined) values.set(name, field.value);
    }
    const revision = workflow.revision;
    if (!revision)
      errors.push(
        'The server did not provide a workflow revision. Reload the workflow.',
      );
    if (errors.length || limit.value === undefined || !revision) {
      await showFormErrors(errors);
      return;
    }
    const sweepValues = swept.map((name) => parsed.get(name)!.values);
    if (
      requiresExactWrites([...values.values(), ...sweepValues.flat()]) &&
      !supportsExactNumberWrites()
    ) {
      await showFormErrors([
        'Update your browser to send these numbers without changing them.',
      ]);
      return;
    }
    const sweep: SubmitBatchRequest['sweep'] =
      sweepKind === 'seeds'
        ? { seeds: { parameter: seedName, values: sweepValues[0] } }
        : {
            grid: Object.fromEntries(
              swept.map((name, index) => [name, sweepValues[index]]),
            ),
          };
    const body: Readonly<SubmitBatchRequest> = Object.freeze({
      workflow: workflow.id,
      workflow_revision: revision,
      parameters: Object.freeze(Object.fromEntries(values)),
      sweep,
      max_items: limit.value,
    });
    const rows = [...fields.keys()].map((name) => ({
      name,
      value: values.get(name),
      random: random.has(name),
      swept: swept.includes(name),
    }));
    const request = new AbortController();
    controller = request;
    working = true;
    try {
      const preview = await api.previewBatch(body, { signal: request.signal });
      if (request.signal.aborted) return;
      if (preview.workflow_revision !== revision) {
        reloadWorkflow({
          title: 'The workflow changed during preview',
          recovery: 'Review the reloaded workflow before submitting.',
        });
        return;
      }
      if (
        preview.item_count !== preview.items.length ||
        preview.item_count !== count
      ) {
        problem = {
          title: 'The server expanded the sweep differently',
          detail: `The form expected ${count} runs but the server returned ${preview.item_count}. Nothing was submitted.`,
          recovery: 'Check the sweep values and preview again.',
        };
        return;
      }
      review = { body, preview, rows, key: crypto.randomUUID() };
      await tick();
      reviewHeading?.focus();
    } catch (failure) {
      if (request.signal.aborted) return;
      handleRejection(failure, 'Could not preview the batch');
    } finally {
      if (controller === request) {
        working = false;
        controller = undefined;
      }
    }
  }

  async function showFormErrors(errors: string[]) {
    formErrors = errors;
    await tick();
    problemHeading?.focus();
  }

  function isWorkflowChanged(failure: unknown): failure is ApiProblemError {
    return (
      failure instanceof ApiProblemError &&
      failure.status === 409 &&
      failure.type === 'urn:genaimage:problem:workflow-changed'
    );
  }

  function handleRejection(failure: unknown, title: string) {
    if (isWorkflowChanged(failure)) {
      invalidate();
      reloadWorkflow(failure.problem);
      return;
    }
    const { invalid_items, invalid_item_count } =
      failure instanceof ApiProblemError ? failure.problem : {};
    const items =
      (invalid_items as
        components['schemas']['BatchItemProblem'][] | undefined) ?? [];
    problem = {
      ...problemOf(failure, title),
      errors: items.length
        ? items.flatMap(({ index, errors }) =>
            errors.map(({ parameter, reason }) => ({
              key: `Item ${index + 1} · ${parameter}`,
              reason,
            })),
          )
        : undefined,
      detail:
        items.length && typeof invalid_item_count === 'number'
          ? `${invalid_item_count} ${invalid_item_count === 1 ? 'item is' : 'items are'} invalid${invalid_item_count > items.length ? `; the first ${items.length} are listed` : ''}. Nothing was recorded.`
          : problemOf(failure, title).detail,
    };
  }

  function submitReview() {
    if (!review || locked) return;
    void send({
      key: review.key,
      body: review.body,
      itemCount: review.preview.item_count,
    });
  }

  async function send(snapshot: PendingBatch) {
    if (working || recovered.problem) return;
    working = true;
    problem = undefined;
    const request = new AbortController();
    controller = request;
    try {
      if (!pending) {
        try {
          savePendingBatch(snapshot);
        } catch (failure) {
          problem = problemOf(failure, 'Could not save a safe retry');
          return;
        }
      }
      pending = snapshot;
      let id: string;
      try {
        const result = await api.createBatch(snapshot.body, snapshot.key, {
          signal: request.signal,
        });
        if (typeof result?.id !== 'string' || !result.id)
          throw new Error('The server did not return a batch.');
        id = result.id;
      } catch (failure) {
        if (request.signal.aborted) return;
        const status =
          failure instanceof ApiProblemError || failure instanceof ApiHttpError
            ? failure.status
            : undefined;
        if (
          status !== undefined &&
          status >= 400 &&
          status < 500 &&
          status !== 408
        ) {
          clearPendingBatch(snapshot);
          pending = undefined;
          review = undefined;
          handleRejection(failure, 'Could not submit the batch');
        } else
          problem = problemOf(failure, 'The submission outcome is uncertain');
        return;
      }
      clearPendingBatch(snapshot);
      pending = undefined;
      navigateHash(`#/batches/${encodeURIComponent(id)}`);
    } catch (failure) {
      problem = problemOf(failure, 'Could not update the pending batch');
    } finally {
      if (controller === request) {
        working = false;
        controller = undefined;
      }
    }
  }

  const sweepSummary = (body: Readonly<SubmitBatchRequest>) =>
    'seeds' in body.sweep
      ? [[body.sweep.seeds.parameter, body.sweep.seeds.values] as const]
      : Object.entries(body.sweep.grid);
</script>

<h2>Batch run</h2>
<p>
  Submit many runs of {workflow.title} in one reviewed request. Each item becomes
  an ordinary run, and you confirm every item before anything is sent.
</p>
{#if problem}<ProblemDetails {problem} />{/if}
{#if pending}
  <section class="panel" aria-labelledby={`${uid}-pending`}>
    <h3 id={`${uid}-pending`}>Pending batch submission</h3>
    <p>
      The batch may already exist. Check
      <a href="#/jobs?view=batches">Batches in Jobs</a> or retry with the same key.
    </p>
    <p>
      Workflow: <code>{pending.body.workflow}</code> · Revision:
      <code>{pending.body.workflow_revision}</code> · Runs:
      <strong>{pending.itemCount}</strong> · Maximum runs: {pending.body
        .max_items}
    </p>
    <dl>
      {#each sweepSummary(pending.body) as [name, values] (name)}
        <dt>Swept: {name}</dt>
        <dd>{values.map(showValue).join(', ')}</dd>
      {/each}
      {#each Object.entries(pending.body.parameters ?? {}) as [name, value] (name)}
        <dt>{name}</dt>
        <dd>{showValue(value)}</dd>
      {/each}
    </dl>
    {#if working}<p role="status">Submitting the reviewed batch…</p>
    {:else}
      <p>Retrying here sends the identical request with the same key.</p>
      <Button onclick={() => pending && send(pending)}
        >Retry with the same key</Button
      >
    {/if}
  </section>
{:else}
  <form novalidate onsubmit={requestReview}>
    <fieldset class="editor" disabled={locked}>
      <fieldset>
        <legend>Sweep type</legend>
        <label
          ><input
            type="radio"
            name={`${uid}-kind`}
            checked={sweepKind === 'seeds'}
            disabled={!seedNames.length}
            onchange={() => ((sweepKind = 'seeds'), invalidate())}
          /> Seeds</label
        >
        <label
          ><input
            type="radio"
            name={`${uid}-kind`}
            checked={sweepKind === 'grid'}
            onchange={() => ((sweepKind = 'grid'), invalidate())}
          /> Grid of parameter values</label
        >
      </fieldset>
      {#if sweepKind === 'seeds'}
        {#if seedNames.length > 1}
          <Select
            label="Seed parameter"
            options={seedNames.map((name) => ({ value: name, label: name }))}
            bind:value={
              () => seedName, (name) => ((seedName = name ?? ''), invalidate())
            }
          />
        {/if}
        <Input
          label={`Seeds for ${seedName} (one per line)`}
          hint="Each line is one run. Whole numbers are sent exactly, including seeds above 2^53."
          multiline
          rows={6}
          error={showErrors ? parsed.get(seedName)?.errors[0] : undefined}
          bind:value={
            () => seedText, (text) => ((seedText = text), invalidate())
          }
        />
        <div class="inline">
          <Input
            label="Random seeds to add"
            type="text"
            bind:value={randomCount}
          />
          <Button onclick={addRandomSeeds}>Add random seeds</Button>
        </div>
        <p role="status">{seedStatus}</p>
      {:else}
        <fieldset>
          <legend>Parameters to vary</legend>
          {#each gridNames as name (name)}
            <label
              ><input
                type="checkbox"
                checked={chosen.has(name)}
                onchange={(event) =>
                  toggleGrid(name, event.currentTarget.checked)}
              />
              {name}</label
            >
          {/each}
        </fieldset>
        {#each [...chosen].sort() as name (name)}
          <Input
            label={`Values for ${name} (one per line)`}
            hint={`Kind: ${workflow.parameters[name].kind}. Write 2.0 to send a decimal and 2 to send a whole number.`}
            multiline
            rows={4}
            error={showErrors ? parsed.get(name)?.errors[0] : undefined}
            bind:value={
              () => gridTexts.get(name) ?? '',
              (text) => (gridTexts.set(name, text), invalidate())
            }
          />
        {/each}
      {/if}
      <p role="status">
        {#if swept.length && ![...parsed.values()].some((item) => item.errors.length)}
          The sweep makes {count} {count === 1 ? 'run' : 'runs'}.
        {/if}
      </p>
      <Input
        label="Maximum runs accepted"
        hint={`The request is rejected, never truncated, if the sweep makes more runs than this. Server limit: ${maxItemsCap}.`}
        error={showErrors ? describeMaxItems().error : undefined}
        bind:value={
          () => maxItems, (text) => ((maxItemsText = text), invalidate())
        }
      />
      {#if Object.keys(workflow.presets).length}
        <Select
          label="Preset"
          hint="Fills the shared values below. Swept parameters stay under the sweep."
          options={[
            { value: '', label: 'Choose a preset', disabled: true },
            ...Object.keys(workflow.presets).map((name) => ({
              value: name,
              label: name,
            })),
          ]}
          bind:value={() => preset, applyPreset}
        />
      {/if}
      {#snippet groups(items: ReturnType<typeof parameterGroups>)}
        {#each items as [group, parameters] (group)}
          <fieldset>
            <legend>{group || 'Parameters'}</legend>
            {#each parameters as [name, parameter] (name)}
              {#if swept.includes(name)}
                <p>{name} is varied by the sweep.</p>
              {:else}
                {@const field = fields.get(name) ?? initialField(parameter)}
                <ParameterField
                  sharedDefault={false}
                  id={`${uid}-${encodeURIComponent(name)}`}
                  {name}
                  {parameter}
                  {field}
                  error={showErrors
                    ? baseError(name, field)
                    : uploadErrors.get(name)}
                  update={(next) => edit(name, next)}
                  reset={() => edit(name, initialField(parameter))}
                  uploadStatus={(message) => {
                    if (message) uploadErrors.set(name, message);
                    else uploadErrors.delete(name);
                    invalidate();
                  }}
                  workflowId={workflow.id}
                />
              {/if}
            {/each}
          </fieldset>
        {/each}
      {/snippet}
      <h3>Shared values</h3>
      {@render groups([
        ...parameterGroups(workflow, false),
        ...parameterGroups(workflow, true),
      ])}
      <Button type="submit" variant="primary" disabled={working}
        >Preview batch</Button
      >
    </fieldset>
  </form>
  {#if formErrors.length}
    <div class="panel" role="alert">
      <h3 tabindex="-1" bind:this={problemHeading}>
        Fix these before previewing
      </h3>
      <ul>
        {#each formErrors as message, index (index)}<li>{message}</li>{/each}
      </ul>
    </div>
  {/if}
  {#if working && !review}<p role="status">Checking every item…</p>{/if}
  {#if review}
    <BatchPreview
      {review}
      {workflow}
      submitting={working}
      onsubmit={submitReview}
      bind:heading={reviewHeading}
    />
  {/if}
{/if}

<style>
  .editor {
    display: grid;
    border: 0;
    margin: 0;
    padding: 0;
    gap: var(--space-3);
    justify-items: start;
    max-width: 50rem;
  }
  .editor > :global(*) {
    min-width: 0;
    width: 100%;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
  }
  label {
    display: block;
  }
  .inline {
    display: flex;
    align-items: end;
    gap: var(--space-3);
  }
  .panel {
    max-width: 50rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
  }
  dt {
    font-weight: 600;
  }
  dd {
    margin: 0 0 0.6rem;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
</style>
