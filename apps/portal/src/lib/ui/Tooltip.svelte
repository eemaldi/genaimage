<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    text,
    children,
  }: {
    text: string;
    children: Snippet<[{ 'aria-describedby': string }]>;
  } = $props();
  const id = $props.id();
  let hovering = $state(false);
  let focused = $state(false);
  let dismissed = $state(false);
  let visible = $derived((hovering || focused) && !dismissed);
  let anchor: HTMLSpanElement;
  let bubble: HTMLSpanElement;
  let shift = $state(0);

  $effect(() => {
    if (!visible) return;
    function position() {
      const { left } = anchor.getBoundingClientRect();
      const { width } = bubble.getBoundingClientRect();
      const limit = Math.max(0, document.documentElement.clientWidth - width);
      shift = Math.min(Math.max(left, 0), limit) - left;
    }
    position();
    const observer =
      typeof ResizeObserver === 'undefined'
        ? undefined
        : new ResizeObserver(position);
    observer?.observe(bubble);
    window.addEventListener('resize', position);
    window.addEventListener('scroll', position, true);
    return () => {
      observer?.disconnect();
      window.removeEventListener('resize', position);
      window.removeEventListener('scroll', position, true);
    };
  });

  function escape(event: KeyboardEvent) {
    if (event.key === 'Escape' && visible) {
      dismissed = true;
      event.preventDefault();
      event.stopPropagation();
    }
  }

  function show() {
    if (!hovering && !focused) dismissed = false;
  }
</script>

<svelte:window onkeydowncapture={escape} />
<!-- svelte-ignore a11y_no_static_element_interactions (wrapper only tracks pointer and focus entry; the trigger owns semantics) -->
<span
  class="ui-tooltip"
  bind:this={anchor}
  onpointerenter={(event) => {
    if (event.pointerType === 'touch') return;
    show();
    hovering = true;
  }}
  onpointerleave={() => (hovering = false)}
  onfocusin={() => {
    show();
    focused = true;
  }}
  onfocusout={(event) => {
    if (
      !(event.relatedTarget instanceof Node) ||
      !event.currentTarget.contains(event.relatedTarget)
    )
      focused = false;
  }}
>
  {@render children({ 'aria-describedby': id })}
  <span
    class="ui-tooltip-bubble"
    role="tooltip"
    {id}
    hidden={!visible}
    bind:this={bubble}
    style:--tooltip-shift={`${shift}px`}><span>{text}</span></span
  >
</span>
