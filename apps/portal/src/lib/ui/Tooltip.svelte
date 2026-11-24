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
  <span class="ui-tooltip-bubble" role="tooltip" {id} hidden={!visible}
    ><span>{text}</span></span
  >
</span>
