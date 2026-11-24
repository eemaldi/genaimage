<script lang="ts">
  import type { Snippet } from 'svelte';
  import { untrack } from 'svelte';
  import Button from './Button.svelte';

  let {
    open = $bindable(false),
    title,
    description,
    returnFocus,
    children,
  }: {
    open?: boolean;
    title: string;
    description?: string;
    returnFocus?: () => void;
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
      if (
        opener instanceof HTMLElement &&
        opener !== document.body &&
        isAvailable(opener)
      ) {
        opener.focus();
        if (document.activeElement === opener) return;
      }
      returnFocus?.();
    };
  });

  function isAvailable(element: HTMLElement) {
    return (
      element.isConnected &&
      !element.matches(':disabled, input[type="hidden"]') &&
      !element.closest('[hidden], [inert]') &&
      (element.checkVisibility?.({ visibilityProperty: true }) ?? true)
    );
  }

  const editable =
    '[contenteditable=""], [contenteditable="true" i], [contenteditable="plaintext-only" i]';

  const nativeMedia = 'video[controls], audio[controls]';

  function isTabStop(element: HTMLElement) {
    if (element.tabIndex >= 0) return true;
    if (element.hasAttribute('tabindex')) return false;
    if (element.matches(nativeMedia)) return true;
    if (!element.matches(editable)) return false;
    // Implicit editing hosts are in the native order even when tabIndex is -1.
    const ancestor = element.parentElement?.closest(
      `${editable}, [contenteditable="false" i]`,
    );
    return !ancestor?.matches(editable);
  }

  function groupedRadio(element: Element | null): element is HTMLInputElement {
    return (
      element instanceof HTMLInputElement &&
      element.type === 'radio' &&
      element.name !== ''
    );
  }

  function handleKey(event: KeyboardEvent) {
    if (event.defaultPrevented || event.key !== 'Tab') return;
    // Native controls live in a user-agent shadow tree. The host is reported
    // as active even as focus moves among several internal controls.
    if (document.activeElement?.matches(nativeMedia)) return;
    const candidates = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        `button, a[href], input, select, textarea, summary, [tabindex], [contenteditable], ${nativeMedia}`,
      ),
    ).filter((el) => isTabStop(el) && isAvailable(el));
    const radios = candidates.filter(groupedRadio);
    function stopFor(element: Element | null) {
      if (!groupedRadio(element)) return element;
      const group = radios.filter(
        (radio) => radio.name === element.name && radio.form === element.form,
      );
      return group.find((radio) => radio.checked) ?? group[0];
    }
    const order = (element: HTMLElement) =>
      element.tabIndex > 0 ? element.tabIndex : Infinity;
    const stops = candidates
      .filter((el) => stopFor(el) === el)
      .sort((a, b) => order(a) - order(b));
    const active = stopFor(document.activeElement);
    const index = stops.findIndex((stop) => stop === active);
    const next = index + (event.shiftKey ? -1 : 1);
    const nextStop = stops[next];
    if (index === -1 || !nextStop) {
      event.preventDefault();
      (event.shiftKey ? stops.at(-1) : stops[0])?.focus();
    } else if (groupedRadio(nextStop) && !nextStop.checked) {
      // Native reverse Tab can enter an unchecked group at its last radio.
      event.preventDefault();
      nextStop.focus();
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
