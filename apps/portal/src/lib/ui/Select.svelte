<script lang="ts">
  import Field from './Field.svelte';
  import type { FieldProps, NativeFieldProps } from './fields';

  let {
    id,
    label,
    hint,
    error,
    value = $bindable(),
    options,
    ...native
  }: FieldProps &
    NativeFieldProps & {
      value?: string;
      options: readonly { value: string; label: string; disabled?: boolean }[];
    } = $props();
</script>

<Field {id} {label} {hint} {error}>
  {#snippet children(field)}
    <select class="ui-control" {...native} {...field} bind:value>
      {#each options as option (option.value)}
        <option value={option.value} disabled={option.disabled}
          >{option.label}</option
        >
      {/each}
    </select>
  {/snippet}
</Field>
