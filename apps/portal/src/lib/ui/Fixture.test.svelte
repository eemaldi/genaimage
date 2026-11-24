<script lang="ts">
  import Button from './Button.svelte';
  import Dialog from './Dialog.svelte';
  import Tabs from './Tabs.svelte';
  import Tooltip from './Tooltip.svelte';

  let {
    kind,
    checkedRadio,
    editorTabIndex,
  }: {
    kind: 'dialog' | 'radio' | 'editor' | 'tabs' | 'tooltip' | 'tooltip-edges';
    checkedRadio?: boolean;
    editorTabIndex?: number;
  } = $props();
  let open = $state(false);
</script>

{#if kind === 'dialog' || kind === 'radio' || kind === 'editor'}
  <Button onclick={() => (open = true)}>Open example</Button>
  <Dialog bind:open title="Example" description="An example dialog">
    <Button disabled>Disabled</Button>
    {#if kind === 'radio'}
      <label
        ><input type="radio" name="quality" checked={checkedRadio} /> Draft</label
      >
      <label><input type="radio" name="quality" /> Final</label>
    {:else if kind === 'editor'}
      <div
        role="textbox"
        aria-label="Prompt editor"
        contenteditable="true"
        tabindex={editorTabIndex}
      >
        <span contenteditable="true">Edit the prompt</span>
      </div>
    {:else}
      <Button>Last action</Button>
    {/if}
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
{:else if kind === 'tooltip-edges'}
  <div style="display: flex; justify-content: space-between">
    {#each ['Left', 'Right'] as edge (edge)}
      <Tooltip text="Enough tooltip text to take up space">
        {#snippet children(attrs)}<Button {...attrs}>{edge}</Button>{/snippet}
      </Tooltip>
    {/each}
  </div>
{:else}
  <Tooltip text="Extra context">
    {#snippet children(attrs)}<Button {...attrs}>Information</Button>{/snippet}
  </Tooltip>
  <Button>Next action</Button>
{/if}
