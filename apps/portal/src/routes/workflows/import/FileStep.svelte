<script lang="ts">
  import Button from '../../../lib/ui/Button.svelte';
  import { maxGraphBytes } from './model';

  let {
    fileName,
    choose,
  }: {
    fileName?: string;
    choose: (file: File) => void;
  } = $props();
  let over = $state(false);
  let input = $state<HTMLInputElement>();
  let notice = $state('');

  function pick(files: FileList | null | undefined) {
    const file = files?.[0];
    notice =
      files && files.length > 1
        ? `Only one file is imported at a time. Using ${file?.name}; the other ${files.length - 1} ignored.`
        : '';
    if (file) choose(file);
  }
</script>

<p>
  Choose a ComfyUI workflow exported with <strong>Workflow, Export (API)</strong
  >. The file is read in your browser and analysed by the local server. Nothing
  is saved until the last step. The limit is {maxGraphBytes / 1024 / 1024} MiB.
</p>
<div
  class="drop"
  class:over
  role="group"
  aria-label="Workflow file"
  ondragover={(event) => {
    event.preventDefault();
    over = true;
  }}
  ondragleave={() => (over = false)}
  ondrop={(event) => {
    event.preventDefault();
    over = false;
    pick(event.dataTransfer?.files);
  }}
>
  <p>Drop an API-format JSON file here, or</p>
  <Button variant="primary" onclick={() => input?.click()}
    >Choose a JSON file</Button
  >
  <input
    bind:this={input}
    class="visually-hidden"
    type="file"
    accept=".json,application/json"
    aria-label="Workflow JSON file"
    tabindex="-1"
    onchange={(event) => {
      pick(event.currentTarget.files);
      event.currentTarget.value = '';
    }}
  />
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if fileName}<p>Selected: <code>{fileName}</code></p>{/if}
</div>

<style>
  .drop {
    display: grid;
    justify-items: start;
    gap: 0.6rem;
    padding: 1.2rem;
    border: 0.15rem dashed var(--border, currentColor);
    border-radius: 0.5rem;
  }
  .drop.over {
    background: var(--subtle);
    border-color: var(--accent);
  }
  .drop p {
    margin: 0;
  }
</style>
