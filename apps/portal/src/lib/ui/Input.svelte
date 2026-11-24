<script lang="ts">
  import type { HTMLInputAttributes } from 'svelte/elements';
  import Field from './Field.svelte';
  import type { FieldProps, NativeFieldProps } from './fields';

  let {
    id,
    label,
    hint,
    error,
    value = $bindable(''),
    multiline = false,
    rows = 4,
    placeholder,
    maxlength,
    autocomplete,
    readonly,
    type = 'text',
    ...native
  }: FieldProps &
    NativeFieldProps & {
      value?: string;
      multiline?: boolean;
      rows?: number;
      placeholder?: string;
      maxlength?: number;
      autocomplete?: HTMLInputAttributes['autocomplete'];
      readonly?: boolean;
      type?: HTMLInputAttributes['type'];
    } = $props();
</script>

<Field {id} {label} {hint} {error}>
  {#snippet children(field)}
    {#if multiline}
      <textarea
        class="ui-control"
        {...native}
        {...field}
        bind:value
        {rows}
        {placeholder}
        {maxlength}
        {autocomplete}
        {readonly}
      ></textarea>
    {:else}
      <input
        class="ui-control"
        {type}
        {...native}
        {...field}
        bind:value
        {placeholder}
        {maxlength}
        {autocomplete}
        {readonly}
      />
    {/if}
  {/snippet}
</Field>
