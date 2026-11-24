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
    hint?: string;
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
  const hintId = `${uid}-hint`;
  const errorId = `${uid}-error`;
</script>

<div class="ui-field">
  <label for={id ?? uid}>{label}</label>
  {@render children({
    id: id ?? uid,
    'aria-describedby':
      [hint && hintId, error && errorId].filter(Boolean).join(' ') || undefined,
    'aria-invalid': error ? true : undefined,
  })}
  {#if hint}<p id={hintId}>{hint}</p>{/if}
  {#if error}<p id={errorId} class="ui-field-error">{error}</p>{/if}
</div>
