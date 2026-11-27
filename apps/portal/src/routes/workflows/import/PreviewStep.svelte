<script lang="ts">
  import { BindingFloat } from '../../../lib/api/client';
  import Select from '../../../lib/ui/Select.svelte';
  import ParameterField from '../../generate/ParameterField.svelte';
  import {
    initialField,
    parameterGroups,
    presetField,
    type FieldState,
    type Parameter,
    type Workflow,
  } from '../../generate/model';
  import type { Json } from './model';

  let { manifest }: { manifest: Json } = $props();
  let overrides = $state<Record<string, FieldState>>({});
  let chosen = $state('');
  let advancedOpen = $state(false);

  const approximate = (value: unknown) =>
    value instanceof BindingFloat
      ? value.value
      : typeof value === 'bigint'
        ? Number(value)
        : typeof value === 'number'
          ? value
          : undefined;

  let workflow = $derived.by(() => {
    const parameters: Record<string, Parameter> = {};
    for (const [name, raw] of Object.entries(
      (manifest.parameters ?? {}) as Record<string, Json>,
    )) {
      parameters[name] = {
        ...raw,
        description: typeof raw.description === 'string' ? raw.description : '',
        required: raw.required === true,
        minimum: approximate(raw.minimum),
        maximum: approximate(raw.maximum),
        step: approximate(raw.step),
      } as Parameter;
    }
    return { ...manifest, parameters } as unknown as Workflow;
  });
  let presets = $derived(
    Object.keys((manifest.presets ?? {}) as Record<string, unknown>),
  );
  let basic = $derived(parameterGroups(workflow, false));
  let advanced = $derived(parameterGroups(workflow, true));

  function applyPreset(name: string) {
    chosen = name;
    const values = ((manifest.presets ?? {}) as Record<string, Json>)[name];
    const next: Record<string, FieldState> = {};
    for (const [key, parameter] of Object.entries(workflow.parameters)) {
      if (values && key in values)
        next[key] = presetField(
          parameter,
          overrides[key] ?? initialField(parameter),
          values[key],
        );
    }
    overrides = next;
  }
</script>

<p>
  This is how the Generate form will look. It is only a preview: nothing is
  submitted, uploaded or sent to the assistant.
</p>
<p class="meta">
  <strong>{String(manifest.title ?? '')}</strong>
  (<code>{String(manifest.id ?? '')}</code>)
</p>

{#if presets.length}
  <Select
    label="Preset"
    bind:value={
      () => chosen,
      (value) =>
        value ? applyPreset(value) : ((chosen = ''), (overrides = {}))
    }
    options={[
      { value: '', label: 'No preset' },
      ...presets.map((name) => ({ value: name, label: name })),
    ]}
  />
{/if}

{#snippet groups(items: ReturnType<typeof parameterGroups>)}
  {#each items as [group, parameters] (group)}
    <fieldset>
      <legend>{group || 'Parameters'}</legend>
      {#each parameters as [name, parameter] (name)}
        <ParameterField
          id={`preview-${name}`}
          sharedDefault={false}
          {name}
          {parameter}
          field={overrides[name] ?? initialField(parameter)}
          update={(next) => (overrides[name] = next)}
          reset={() => delete overrides[name]}
          uploadStatus={() => {}}
          workflowId={String(manifest.id ?? '')}
          preview
        />
      {/each}
    </fieldset>
  {/each}
{/snippet}

<form class="preview" onsubmit={(event) => event.preventDefault()}>
  {#if !basic.length && !advanced.length}
    <p>
      No parameters are exposed. The Generate form will only offer the run
      controls.
    </p>
  {/if}
  {@render groups(basic)}
  {#if advanced.length}
    <details bind:open={advancedOpen}>
      <summary>Advanced</summary>
      {@render groups(advanced)}
    </details>
  {/if}
</form>
