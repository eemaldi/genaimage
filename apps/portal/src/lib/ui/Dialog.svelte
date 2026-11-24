<script lang="ts">
  import type { Snippet } from 'svelte';
  import { untrack } from 'svelte';
  import Button from './Button.svelte';

  let {
    open = $bindable(false),
    title,
    description,
    children,
  }: {
    open?: boolean;
    title: string;
    description?: string;
    children?: Snippet;
  } = $props();
  const uid = $props.id();
  let dialog: HTMLDialogElement;

  $effect(() => {
    if (!open) return;
    // showModal puts the dialog in the top layer and makes the background inert.
    const opener = document.activeElement;
    untrack(() => dialog.showModal());
    return () => {
      if (dialog.open) dialog.close();
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  });

  function handleKey(event: KeyboardEvent) {
    if (event.defaultPrevented || event.key !== 'Tab') return;
    const stops = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'button, a[href], input, select, textarea, summary, [tabindex], [contenteditable]',
      ),
    ).filter(
      (el) =>
        el.tabIndex >= 0 &&
        !el.matches(':disabled, input[type="hidden"]') &&
        !el.closest('[hidden], [inert]') &&
        (el.checkVisibility?.({ visibilityProperty: true }) ?? true),
    );
    const first = stops[0];
    const last = stops.at(-1);
    const active = document.activeElement;
    if (
      !stops.some((el) => el === active) ||
      active === (event.shiftKey ? first : last)
    ) {
      event.preventDefault();
      (event.shiftKey ? last : first)?.focus();
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="ui-dialog"
  aria-modal="true"
  aria-labelledby={`${uid}-title`}
  aria-describedby={description ? `${uid}-description` : undefined}
  onkeydown={handleKey}
  oncancel={(event) => {
    event.preventDefault();
    open = false;
  }}
  onclose={() => {
    if (!dialog.open) open = false;
  }}
>
  <div class="ui-dialog-header">
    <h2 id={`${uid}-title`}>{title}</h2>
    <Button onclick={() => (open = false)}>Close</Button>
  </div>
  {#if description}<p id={`${uid}-description`}>{description}</p>{/if}
  {#if open}{@render children?.()}{/if}
</dialog>
