<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    id,
    label,
    hint,
    error,
    children,
  }: {
    id?: string;
    label: string;
    hint?: string | Array<string | null | undefined>;
    error?: string;
    children: Snippet<
      [
        {
          id: string;
          'aria-describedby'?: string;
          'aria-invalid'?: true;
        },
      ]
    >;
  } = $props();
  const uid = $props.id();
  const hints = $derived(
    (Array.isArray(hint) ? hint : [hint]).filter((line) => !!line),
  );
  const hintIds = $derived(hints.map((_, index) => `${uid}-hint-${index}`));
  const errorId = `${uid}-error`;
</script>

<div class="ui-field">
  <label for={id ?? uid}>{label}</label>
  {@render children({
    id: id ?? uid,
    'aria-describedby':
      [...hintIds, error && errorId].filter(Boolean).join(' ') || undefined,
    'aria-invalid': error ? true : undefined,
  })}
  {#each hints as line, index (index)}<p id={hintIds[index]}>{line}</p>{/each}
  {#if error}<p id={errorId} class="ui-field-error">{error}</p>{/if}
</div>
