<script lang="ts">
  import { tick } from 'svelte';
  import Button from './Button.svelte';
  import { dismissToast, toasts } from './toasts';

  let { returnFocus }: { returnFocus?: () => void } = $props();
  let list: HTMLUListElement;

  async function dismiss(id: number, button: HTMLButtonElement) {
    const restore = button === document.activeElement;
    const item = button.closest('li');
    const neighbour = item?.nextElementSibling ?? item?.previousElementSibling;
    dismissToast(id);
    if (!restore) return;
    await tick();
    if (document.activeElement !== document.body) return;
    const next = neighbour?.querySelector('button');
    if (next?.isConnected) next.focus();
    else if (returnFocus) returnFocus();
    else list.focus();
  }
</script>

<div class="visually-hidden" role="status" aria-atomic="false">
  {#each $toasts.filter((item) => item.kind === 'info') as toast (toast.id)}
    <div>{toast.message}</div>
  {/each}
</div>
<div class="visually-hidden" role="alert" aria-atomic="false">
  {#each $toasts.filter((item) => item.kind === 'error') as toast (toast.id)}
    <div>{toast.message}</div>
  {/each}
</div>
<ul
  class="ui-toasts"
  aria-label="Notifications"
  tabindex="-1"
  bind:this={list}
  hidden={!$toasts.length && returnFocus !== undefined}
>
  {#each $toasts as toast (toast.id)}
    <li class:error-toast={toast.kind === 'error'}>
      <span
        >{#if toast.kind === 'error'}<strong>Error:</strong>
        {/if}{toast.message}</span
      >
      <Button
        aria-label={`Dismiss notification: ${toast.message}`}
        onclick={(event) => dismiss(toast.id, event.currentTarget)}
        >Dismiss</Button
      >
    </li>
  {/each}
</ul>
