<script lang="ts">
  import type { HTMLButtonAttributes } from 'svelte/elements';

  let {
    type = 'button',
    variant = 'secondary',
    children,
    ...attrs
  }: HTMLButtonAttributes & {
    variant?: 'primary' | 'secondary';
  } = $props();
</script>

<button
  {...attrs}
  {type}
  class={['ui-button', attrs.class]}
  class:primary={variant === 'primary'}
>
  {@render children?.()}
</button>
