<script lang="ts">
  import { BindingFloat } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import Field from '../../lib/ui/Field.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import Select from '../../lib/ui/Select.svelte';
  import Slider from '../../lib/ui/Slider.svelte';
  import ImageField from './ImageField.svelte';
  import {
    bindingError,
    numericDraft,
    requirementLabel,
    sameValue,
    type FieldState,
    type Parameter,
    type Scalar,
  } from './model';

  let {
    id,
    sharedDefault,
    name,
    parameter,
    field,
    error,
    limit,
    imageEpoch,
    update,
    reset,
    uploadStatus,
  }: {
    id: string;
    sharedDefault: boolean;
    name: string;
    parameter: Parameter;
    field: FieldState;
    error?: string;
    limit?: number;
    imageEpoch?: number;
    update: (next: FieldState) => void;
    reset: () => void;
    uploadStatus: (message?: string) => void;
  } = $props();

  let needed = $derived(parameter.required);
  let hint = $derived(
    [
      parameter.description,
      `${requirementLabel(parameter)}.`,
      parameter.kind !== 'image_upload' && parameter.minimum != null
        ? `Minimum: ${parameter.minimum}.`
        : '',
      parameter.kind !== 'image_upload' && parameter.maximum != null
        ? `Maximum: ${parameter.maximum}.`
        : '',
      parameter.step != null ? `Suggested increment: ${parameter.step}.` : '',
    ]
      .filter(Boolean)
      .join(' '),
  );
  let numeric = $derived(
    ['seed', 'integer', 'number'].includes(parameter.kind),
  );
  let allowed = $derived(parameter.options ?? parameter.allowed_values);
  let select = $derived(
    parameter.widget === 'select' ||
      (parameter.widget == null &&
        !!allowed &&
        parameter.kind !== 'image_upload'),
  );
  let choices = $derived.by(() => {
    if (!select) return [];
    const source =
      allowed ?? (parameter.kind === 'boolean' ? [true, false] : []);
    const values = [...source, ...(parameter.allowed_values ?? [])].filter(
      (value): value is Scalar =>
        typeof value === 'string' ||
        typeof value === 'number' ||
        typeof value === 'bigint' ||
        typeof value === 'boolean' ||
        value instanceof BindingFloat,
    );
    const unique = values.filter(
      (value, index) =>
        values.findIndex((other) => sameValue(value, other)) === index,
    );
    if (
      field.value !== undefined &&
      !unique.some((value) => sameValue(value, field.value))
    )
      unique.push(field.value);
    return unique;
  });
  let step: number | 'any' = $derived(
    parameter.step ?? (parameter.kind === 'number' ? 'any' : 1),
  );
  let slider = $derived.by(() => {
    const value =
      field.value instanceof BindingFloat ? field.value.value : field.value;
    const { minimum: min, maximum: max } = parameter;
    if (
      parameter.widget !== 'slider' ||
      typeof value !== 'number' ||
      min == null ||
      max == null ||
      min >= max ||
      Math.abs(min) > Number.MAX_SAFE_INTEGER ||
      Math.abs(max) > Number.MAX_SAFE_INTEGER ||
      bindingError(parameter, field)
    )
      return;
    if (step !== 'any') {
      const ticks = (value - min) / step;
      if (Math.abs(ticks - Math.round(ticks)) > 1e-9) return;
    }
    if (
      parameter.kind !== 'number' &&
      (!Number.isInteger(min) || !Number.isInteger(max))
    )
      return;
    return { min, max, value };
  });

  function edit(value?: Scalar) {
    update({ value, seedMode: field.seedMode, source: 'user' });
  }
</script>

{#snippet numericInput(inputId: string, label: string)}
  <Field id={inputId} {label} {hint} {error}>
    {#snippet children(attrs)}
      <input
        {...attrs}
        class="ui-control"
        type="text"
        inputmode={parameter.kind === 'number' ? 'decimal' : 'numeric'}
        required={needed}
        placeholder={parameter.placeholder ?? undefined}
        value={field.draft ?? String(field.value ?? '')}
        oninput={(event) =>
          update({
            ...numericDraft(parameter, event.currentTarget.value),
            source: 'user',
            seedMode: field.seedMode,
          })}
      />
    {/snippet}
  </Field>
{/snippet}

<div class:highlighted={field.highlighted} class="parameter">
  {#if parameter.kind === 'seed'}
    <fieldset class="seed-mode">
      <legend>{name} mode</legend>
      <label
        ><input
          type="radio"
          name={`${id}-mode`}
          checked={field.seedMode !== 'random'}
          onchange={() =>
            update({ ...field, seedMode: 'locked', highlighted: false })}
        /> Lock seed</label
      >
      <label
        ><input
          type="radio"
          name={`${id}-mode`}
          id={field.seedMode === 'random' ? id : undefined}
          checked={field.seedMode === 'random'}
          aria-describedby={field.seedMode === 'random' && error
            ? `${id}-random-error`
            : undefined}
          onchange={() =>
            update({ ...field, seedMode: 'random', highlighted: false })}
        /> Randomise seed</label
      >
    </fieldset>
  {/if}
  {#if field.seedMode === 'random'}
    <p>
      {[hint, 'A concrete seed will be chosen when you request review.']
        .filter(Boolean)
        .join(' ')}
    </p>
    {#if error}<p class="random-error" id={`${id}-random-error`}>
        {error}
      </p>{/if}
  {:else if parameter.kind === 'image_upload'}
    {#key imageEpoch}
      <ImageField
        {id}
        label={name}
        {hint}
        {error}
        required={needed}
        {limit}
        value={typeof field.value === 'string' ? field.value : undefined}
        change={edit}
        status={uploadStatus}
      />
    {/key}
  {:else if select && (allowed?.length || parameter.kind === 'boolean')}
    <Select
      {id}
      label={name}
      {hint}
      {error}
      required={needed}
      options={[
        {
          value: '',
          label: 'Choose a value',
        },
        ...choices.map((value, index) => ({
          value: String(index),
          label:
            typeof value === 'boolean' ? (value ? 'Yes' : 'No') : String(value),
        })),
      ]}
      bind:value={
        () => {
          const index =
            field.value === undefined
              ? -1
              : choices.findIndex((value) => sameValue(value, field.value));
          return index < 0 ? '' : String(index);
        },
        (index) => edit(index ? choices[Number(index)] : undefined)
      }
    />
    {#if !parameter.allowed_values && parameter.kind !== 'boolean'}
      {#if numeric}
        {@render numericInput(`${id}-custom`, `${name} custom value`)}
      {:else}
        <Input
          id={`${id}-custom`}
          label={`${name} custom value`}
          {error}
          bind:value={() => String(field.value ?? ''), edit}
        />
      {/if}
    {/if}
  {:else if parameter.kind === 'boolean'}
    <Field {id} label={name} {hint} {error}>
      {#snippet children(attrs)}
        <input
          {...attrs}
          type="checkbox"
          checked={field.value === true}
          indeterminate={field.value === undefined}
          aria-required={needed}
          onchange={(event) => edit(event.currentTarget.checked)}
        />
      {/snippet}
    </Field>
    <p>{field.value === undefined ? 'Not set' : field.value ? 'Yes' : 'No'}</p>
  {:else if numeric}
    {@render numericInput(id, name)}
    {#if slider}
      <Slider
        label={`${name} slider`}
        min={slider.min}
        max={slider.max}
        {step}
        bind:value={() => slider?.value, edit}
      />
    {:else if parameter.widget === 'slider'}
      <p>
        Enter a value directly. The slider is only available when it can show it
        exactly.
      </p>
    {/if}
  {:else}
    <Input
      {id}
      label={name}
      {hint}
      {error}
      required={needed}
      placeholder={parameter.placeholder ?? undefined}
      multiline={parameter.widget === 'textarea'}
      bind:value={() => String(field.value ?? ''), edit}
    />
  {/if}

  {#if field.source === 'default'}
    <p>
      {sharedDefault ? 'Shared default' : 'Manifest default'}: {field.value ===
      ''
        ? '(empty text)'
        : String(field.value)}
    </p>
  {:else if field.source === 'preset'}
    <p>
      {field.highlighted
        ? 'Changed by preset'
        : 'From preset'}{#if field.value === ''}: (empty text){/if}
    </p>
  {/if}
  {#if (field.source && field.source !== 'default') || sharedDefault || field.seedMode === 'random' || (parameter.kind === 'image_upload' && !!error)}
    <Button onclick={reset} aria-label={`Reset ${name}`}>Reset</Button>
  {/if}
</div>

<style>
  .parameter {
    display: grid;
    justify-items: start;
    gap: 0.4rem;
    padding: 0.6rem;
    min-width: 0;
  }
  .parameter :global(.ui-field) {
    width: 100%;
    min-width: 0;
  }
  .parameter p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .highlighted {
    background: var(--subtle);
    border-inline-start: 0.2rem solid var(--accent);
  }
  .seed-mode {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 1rem;
  }
  .seed-mode label {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-height: var(--tap-target);
  }
  .random-error {
    color: var(--danger);
  }
</style>
