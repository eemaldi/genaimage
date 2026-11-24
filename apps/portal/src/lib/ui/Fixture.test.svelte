<script lang="ts">
  import Button from './Button.svelte';
  import Dialog from './Dialog.svelte';
  import Tabs from './Tabs.svelte';
  import Tooltip from './Tooltip.svelte';

  let { kind }: { kind: 'dialog' | 'tabs' | 'tooltip' } = $props();
  let open = $state(false);
</script>

{#if kind === 'dialog'}
  <Button onclick={() => (open = true)}>Open example</Button>
  <Dialog bind:open title="Example" description="An example dialog">
    <Button disabled>Disabled</Button>
    <Button>Last action</Button>
  </Dialog>
{:else if kind === 'tabs'}
  <Tabs
    label="Options"
    tabs={[
      { id: 'one', label: 'One' },
      { id: 'two', label: 'Two' },
      { id: 'three', label: 'Three' },
    ]}
  >
    {#snippet children(tab)}Content for {tab.label}{/snippet}
  </Tabs>
{:else}
  <Tooltip text="Extra context">
    {#snippet children(attrs)}<Button {...attrs}>Information</Button>{/snippet}
  </Tooltip>
  <Button>Next action</Button>
{/if}
