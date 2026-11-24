<script lang="ts">
  import Field from './Field.svelte';
  import type { FieldProps } from './fields';

  let {
    id,
    label,
    hint,
    error,
    value = $bindable(),
    min = 0,
    max = 100,
    step = 1,
    name,
    disabled,
  }: FieldProps & {
    value?: number;
    min?: number;
    max?: number;
    step?: number | 'any';
    name?: string;
    disabled?: boolean;
  } = $props();
</script>

<Field {id} {label} {hint} {error}>
  {#snippet children(field)}
    <div class="ui-slider">
      <input
        {...field}
        type="range"
        bind:value
        {min}
        {max}
        {step}
        {name}
        {disabled}
      />
      <span aria-hidden="true">{value}</span>
    </div>
  {/snippet}
</Field>
