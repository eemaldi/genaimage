import { tick } from 'svelte';

export function keepFocusAcross(element: HTMLElement) {
  const focused = document.activeElement === element;
  return async () => {
    if (!focused) return;
    await tick();
    const active = document.activeElement;
    if (!active || active === document.body) element.focus();
  };
}
