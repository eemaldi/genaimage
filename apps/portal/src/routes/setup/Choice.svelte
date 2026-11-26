<script lang="ts">
  import Field from '../../lib/ui/Field.svelte';

  let {
    label,
    hint,
    value,
    options,
    onchoose,
  }: {
    label: string;
    hint?: string;
    value: string;
    options: readonly { value: string; label: string }[];
    onchoose: (value: string) => void;
  } = $props();
</script>

<Field {label} {hint}>
  {#snippet children(field)}
    <select
      class="ui-control"
      {...field}
      {value}
      onchange={(event) => onchoose(event.currentTarget.value)}
    >
      {#each options as option (option.value)}
        <option value={option.value} selected={option.value === value}
          >{option.label}</option
        >
      {/each}
    </select>
  {/snippet}
</Field>
