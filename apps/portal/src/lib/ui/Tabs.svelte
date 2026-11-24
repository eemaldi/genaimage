<script module lang="ts">
  export type Tab = { id: string; label: string };
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    label,
    tabs,
    value = $bindable(),
    children,
  }: {
    label: string;
    tabs: readonly Tab[];
    value?: string;
    children: Snippet<[Tab]>;
  } = $props();

  const uid = $props.id();
  let selected = $derived(tabs.findIndex((tab) => tab.id === value));
  let active = $derived(Math.max(0, selected));

  function onKeydown(event: KeyboardEvent, index: number) {
    let next: number;
    switch (event.key) {
      case 'ArrowRight':
        next = (index + 1) % tabs.length;
        break;
      case 'ArrowLeft':
        next = (index + tabs.length - 1) % tabs.length;
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = tabs.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    value = tabs[next].id;
    document.getElementById(`${uid}-tab-${next}`)?.focus();
  }
</script>

{#if tabs.length}
  <div class="ui-tablist" role="tablist" aria-label={label}>
    {#each tabs as tab, index (tab.id)}
      <button
        type="button"
        role="tab"
        id={`${uid}-tab-${index}`}
        aria-controls={`${uid}-panel-${index}`}
        aria-selected={active === index}
        tabindex={active === index ? 0 : -1}
        onclick={() => (value = tab.id)}
        onkeydown={(event) => onKeydown(event, index)}>{tab.label}</button
      >
    {/each}
  </div>
  {#each tabs as tab, index (tab.id)}
    <div
      class="ui-tabpanel"
      role="tabpanel"
      tabindex="0"
      id={`${uid}-panel-${index}`}
      aria-labelledby={`${uid}-tab-${index}`}
      hidden={active !== index}
    >
      {@render children(tab)}
    </div>
  {/each}
{/if}
