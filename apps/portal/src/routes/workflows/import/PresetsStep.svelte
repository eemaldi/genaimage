<script lang="ts">
  import Button from '../../../lib/ui/Button.svelte';
  import Input from '../../../lib/ui/Input.svelte';
  import Select from '../../../lib/ui/Select.svelte';
  import {
    nextUid,
    type DraftErrors,
    type ManifestDraft,
    type PresetDraft,
  } from './model';

  let {
    draft = $bindable(),
    errors,
    serverErrors,
    showErrors,
  }: {
    draft: ManifestDraft;
    errors: DraftErrors;
    serverErrors: Record<string, string>;
    showErrors: boolean;
  } = $props();
  let exposed = $derived(draft.parameters.filter((item) => item.exposed));

  function addPreset() {
    draft.presets.push({ uid: nextUid(draft), name: '', values: [] });
  }
  function addValue(preset: PresetDraft) {
    const first = exposed.find(
      (item) =>
        item.kind !== 'image_upload' &&
        !preset.values.some((value) => value.parameter === item.uid),
    );
    if (!first) return;
    preset.values.push({
      uid: nextUid(draft),
      parameter: first.uid,
      text: first.defaultText,
    });
  }
  const kindOf = (uid: number) =>
    exposed.find((item) => item.uid === uid)?.kind ?? 'string';
</script>

<p>
  A preset is a named set of values for exposed parameters. Choosing it on the
  Generate form fills those fields; you still review everything before a run. A
  preset does not approve content.
</p>

{#each draft.presets as preset, index (preset.uid)}
  {@const found = errors.presets.get(preset.uid)}
  <fieldset class="preset">
    <legend>Preset {index + 1}</legend>
    <Input
      label="Preset name"
      error={(showErrors ? found?.name : undefined) ??
        serverErrors[`presets.${preset.name}`]}
      bind:value={preset.name}
    />
    {#each preset.values as entry, at (entry.uid)}
      <div class="row">
        <Select
          label={`Parameter ${at + 1}`}
          bind:value={
            () => String(entry.parameter),
            (value) => {
              entry.parameter = Number(value);
              entry.text = '';
            }
          }
          options={exposed
            .filter((item) => item.kind !== 'image_upload')
            .map((item) => ({ value: String(item.uid), label: item.key }))}
        />
        {#if kindOf(entry.parameter) === 'boolean'}
          <Select
            label={`Value ${at + 1}`}
            error={showErrors ? found?.values.get(entry.uid) : undefined}
            bind:value={entry.text}
            options={[
              { value: '', label: 'Choose a value' },
              { value: 'true', label: 'Yes' },
              { value: 'false', label: 'No' },
            ]}
          />
        {:else}
          <Input
            label={`Value ${at + 1}`}
            error={showErrors ? found?.values.get(entry.uid) : undefined}
            bind:value={entry.text}
          />
        {/if}
        <Button
          onclick={() => preset.values.splice(at, 1)}
          aria-label={`Remove value ${at + 1} from preset ${preset.name || index + 1}`}
          >Remove value</Button
        >
      </div>
    {/each}
    <p>
      <Button
        onclick={() => addValue(preset)}
        disabled={preset.values.length >=
          exposed.filter((item) => item.kind !== 'image_upload').length}
        >Add a value</Button
      >
      <Button onclick={() => draft.presets.splice(index, 1)}
        >Remove preset {preset.name || index + 1}</Button
      >
    </p>
  </fieldset>
{/each}
{#if !draft.presets.length}<p>No presets yet. They are optional.</p>{/if}
<p><Button onclick={addPreset}>Add a preset</Button></p>

<style>
  .row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
    align-items: end;
    gap: 0.6rem;
    margin-block: 0.5rem;
  }
</style>
