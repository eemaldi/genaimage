<script lang="ts">
  import { tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import {
    api,
    BindingFloat,
    supportsExactNumberWrites,
  } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
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

  let { workflow }: { workflow: Workflow } = $props();
  const uid = $props.id();
  const fieldId = (name: string) => `${uid}-${encodeURIComponent(name)}`;
  const fields = new SvelteMap<string, FieldState>(
    untrack(() =>
      Object.entries(workflow.parameters).map(([name, parameter]) => [
        name,
        initialField(parameter),
      ]),
    ),
  );
  const uploadErrors = new SvelteMap<string, string>();
  const imageEpochs = new SvelteMap<string, number>();
  let basic = $derived(parameterGroups(workflow, false));
  let advanced = $derived(parameterGroups(workflow, true));
  let advancedOpen = $state(false);
  let preset = $state('');
  let modified = $state(false);
  let showErrors = $state(false);
  let reviewErrors = $state.raw<Record<string, string>>({});
  let review = $state.raw<Review>();
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

  function applyPreset(name: string | undefined) {
    if (!name || !Object.hasOwn(workflow.presets, name)) return;
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
        resetUpload(key);
      } else if (field.highlighted) {
        fields.set(key, { ...field, highlighted: false });
      }
    }
    preset = name;
    modified = false;
    review = undefined;
    reviewErrors = {};
  }

  function edit(name: string, next: FieldState) {
    fields.set(name, next);
    if (preset) modified = true;
    review = undefined;
    reviewErrors = {};
  }

  function resetUpload(name: string) {
    if (workflow.parameters[name].kind !== 'image_upload') return;
    uploadErrors.delete(name);
    imageEpochs.set(name, (imageEpochs.get(name) ?? 0) + 1);
  }

  function uploadStatus(name: string, message?: string) {
    if (message) uploadErrors.set(name, message);
    else uploadErrors.delete(name);
    review = undefined;
    reviewErrors = {};
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
    showErrors = true;
    review = undefined;
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
        const value = randomSeed(workflow.parameters[name]);
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
    reviewErrors = Object.fromEntries(errors);
    if (errors.size) {
      const first = entries.find(([name]) => errors.has(name))?.[0];
      if (
        entries.some(
          ([name]) => errors.has(name) && workflow.parameters[name].advanced,
        )
      )
        advancedOpen = true;
      await tick();
      if (first !== undefined) document.getElementById(fieldId(first))?.focus();
      return;
    }
    review = Object.fromEntries(
      entries.flatMap(([name]) => {
        const value = values.get(name);
        return value === undefined ? [] : [[name, value] as const];
      }),
    );
    await tick();
    reviewHeading?.focus();
  }
</script>

<h2>{workflow.title}</h2>
{#if workflow.description}<p>{workflow.description}</p>{/if}
<form novalidate onsubmit={requestReview}>
  {#if Object.keys(workflow.presets).length}
    <div class="presets">
      <Select
        label="Preset"
        hint="Choose a preset to fill its parameters. Other fields stay as they are."
        options={[
          { value: '', label: 'Choose a preset', disabled: true },
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
  <Button type="submit" variant="primary">Review parameters</Button>
</form>

{#if review}
  <section class="review" aria-labelledby={`${uid}-review`}>
    <h3 id={`${uid}-review`} tabindex="-1" bind:this={reviewHeading}>
      Review parameters
    </h3>
    <p>No run has been submitted.</p>
    {#if Object.keys(review).length}
      <dl>
        {#each Object.entries(review) as [name, value] (name)}
          <dt>
            {name}{fields.get(name)?.seedMode === 'random'
              ? ' (randomised seed)'
              : ''}
          </dt>
          <dd>{value === '' ? '(empty text)' : String(value)}</dd>
        {/each}
      </dl>
    {:else}<p>No parameters are set.</p>{/if}
  </section>
{/if}

<style>
  form {
    display: grid;
    gap: 1rem;
    justify-items: start;
  }
  form,
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
  dt {
    font-weight: 600;
  }
</style>
