<script lang="ts">
  import Button from '../../../lib/ui/Button.svelte';
  import Input from '../../../lib/ui/Input.svelte';
  import Select from '../../../lib/ui/Select.svelte';
  import Detected from './Detected.svelte';
  import {
    numericKinds,
    widgetChoices,
    type DraftErrors,
    type ManifestDraft,
    type Proposal,
  } from './model';

  let {
    draft = $bindable(),
    proposal,
    errors,
    serverErrors,
    showErrors,
    conflicts,
    notes,
    fixPresets,
  }: {
    draft: ManifestDraft;
    proposal: Proposal;
    errors: DraftErrors;
    serverErrors: Record<string, string>;
    showErrors: boolean;
    conflicts: { preset: string; parameter: string }[];
    notes: string[];
    fixPresets: () => void;
  } = $props();
  const shown = <T,>(value: T) => (showErrors ? value : undefined);
</script>

{#if notes.length}
  <p role="status">
    The importer proposed preset values for parameters it did not define, so
    they were left out: {notes.join('; ')}.
  </p>
{/if}
{#if conflicts.length}
  <div class="conflicts" role="status">
    <ul>
      {#each conflicts as item, at (at)}
        <li>
          Preset {item.preset} uses {item.parameter}. Expose {item.parameter} again
          or remove it from the preset.
        </li>
      {/each}
    </ul>
    <Button onclick={fixPresets}>Fix in step 3</Button>
  </div>
{/if}

<fieldset>
  <legend>Workflow details</legend>
  <Input
    label="Workflow ID"
    hint="Lower-case letters, digits, hyphens and underscores. It names the folder and the Generate link."
    error={shown(errors.id) ?? serverErrors.id}
    bind:value={draft.id}
  />
  <Input label="Title" error={shown(errors.title)} bind:value={draft.title} />
  <Input
    label="Description"
    multiline
    rows={2}
    bind:value={draft.description}
  />
  <Input
    label="Version"
    error={shown(errors.version) ?? serverErrors.version}
    bind:value={draft.version}
  />
</fieldset>

<section aria-labelledby="parameters-heading">
  <h3 id="parameters-heading">Parameters ({draft.parameters.length})</h3>
  <p>
    Exposed parameters become fields on the Generate form. A parameter that is
    not exposed is left out of the manifest, so the workflow value is used. A
    parameter's node and input cannot be changed.
  </p>
  {#each draft.parameters as item (item.uid)}
    {@const found = errors.parameters.get(item.uid) ?? {}}
    {@const remote =
      serverErrors[`parameters.${item.key}`] ??
      serverErrors[`parameters.${item.original}`]}
    <fieldset class="parameter" data-parameter={item.original}>
      <legend>{item.original}</legend>
      <p class="binding">
        Node {item.node}, input <code>{item.input}</code>, kind
        <code>{item.kind}</code>{item.required ? ', required' : ''}. Workflow
        value: {item.defaultText === '' ? 'none' : item.defaultText}.
      </p>
      <label class="expose">
        <input type="checkbox" bind:checked={item.exposed} />
        Expose {item.original} in the form
      </label>
      {#if remote}<p class="remote" role="alert">{remote}</p>{/if}
      {#if item.exposed}
        <div class="grid">
          <Input label="Name" error={shown(found.key)} bind:value={item.key} />
          <Input label="Description" bind:value={item.description} />
          {#if numericKinds.includes(item.kind)}
            <Input
              label="Minimum"
              error={shown(found.minimum)}
              bind:value={item.minimum}
            />
            <Input
              label="Maximum"
              error={shown(found.maximum)}
              bind:value={item.maximum}
            />
            <Input
              label="Step"
              error={shown(found.step)}
              bind:value={item.step}
            />
          {/if}
          {#if widgetChoices(item.kind).length}
            <Select
              label="Widget"
              bind:value={item.widget}
              options={[
                { value: '', label: 'Automatic' },
                ...widgetChoices(item.kind).map((value) => ({
                  value,
                  label: value,
                })),
              ]}
            />
          {/if}
          <Input label="Group" bind:value={item.group} />
          {#if item.kind === 'string'}
            <Input label="Placeholder" bind:value={item.placeholder} />
          {/if}
          {#if item.kind !== 'image_upload' && item.kind !== 'boolean'}
            <Input
              label="Options"
              hint="One per line. Leave empty for free entry."
              multiline
              rows={3}
              error={shown(found.options)}
              bind:value={item.options}
            />
          {/if}
        </div>
        <label class="expose">
          <input type="checkbox" bind:checked={item.advanced} />
          Show under Advanced
        </label>
      {:else}
        <p>Not exposed. The workflow value is used.</p>
      {/if}
    </fieldset>
  {/each}
  {#if !draft.parameters.length}
    <p>
      The importer found no parameters. The workflow runs with its own values.
    </p>
  {/if}
</section>

<Detected {proposal} />

<style>
  fieldset {
    margin-block: 0.8rem;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
    align-items: start;
    gap: 0.6rem;
  }
  .conflicts {
    color: var(--danger);
  }
  .binding {
    margin: 0 0 0.4rem;
    overflow-wrap: anywhere;
  }
  .expose {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    min-height: var(--tap-target);
  }
  .remote {
    color: var(--danger);
  }
</style>
