<script lang="ts">
  import type { Component } from 'svelte';
  import Button from '../ui/Button.svelte';
  import ProblemDetails from '../ui/ProblemDetails.svelte';
  import type { TemplatePickerProps } from './TemplatePicker.svelte';

  let {
    name,
    currentText,
    insert,
  }: {
    name: string;
    currentText: () => string;
    insert: (text: string) => void;
  } = $props();

  let picker = $state<Component<TemplatePickerProps>>();
  let open = $state(false);
  let loadFailed = $state(false);

  async function choose() {
    try {
      picker ??= (await import('./TemplatePicker.svelte')).default;
      open = true;
    } catch {
      loadFailed = true;
    }
  }
</script>

<div class="template-action">
  <Button
    aria-label={`Use a template for ${name}`}
    aria-haspopup="dialog"
    onclick={choose}>Use a template</Button
  >
  {#if loadFailed}
    <ProblemDetails
      problem={{
        title: 'Could not load the template picker',
        recovery: 'Reload the page to try again.',
      }}
    />
  {/if}
</div>
{#if open && picker}
  {@const Picker = picker}
  <Picker {name} {currentText} {insert} bind:open />
{/if}

<style>
  .template-action {
    display: grid;
    gap: var(--space-2);
    justify-items: start;
  }
</style>
