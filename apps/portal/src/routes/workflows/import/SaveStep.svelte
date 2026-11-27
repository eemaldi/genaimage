<script lang="ts">
  import { formatExactJson } from '../../../lib/api/client';
  import type { Json } from './model';

  let {
    manifest,
    graphSha256,
    nodeCount,
  }: { manifest: Json; graphSha256: string; nodeCount: number } = $props();
  const show = (value: unknown) =>
    typeof value === 'string' ? value : formatExactJson(value, 0);
  let parameters = $derived(
    Object.entries((manifest.parameters ?? {}) as Record<string, Json>),
  );
  let presets = $derived(
    Object.entries((manifest.presets ?? {}) as Record<string, Json>),
  );
</script>

<p>
  Saving writes two new files in your user workflows folder, the graph first and
  the manifest last. Nothing existing is overwritten. Check every value below.
</p>

<dl class="summary">
  <dt>Workflow ID</dt>
  <dd><code>{String(manifest.id)}</code></dd>
  <dt>Files</dt>
  <dd>
    <code>{String(manifest.id)}/api-workflow.json</code> and
    <code>{String(manifest.id)}/workflow.genaimage.json</code>
  </dd>
  <dt>Title</dt>
  <dd>{String(manifest.title ?? '')}</dd>
  <dt>Version</dt>
  <dd>{String(manifest.version ?? '')}</dd>
  {#if manifest.description}
    <dt>Description</dt>
    <dd>{String(manifest.description)}</dd>
  {/if}
  <dt>Graph</dt>
  <dd>{nodeCount} nodes, SHA-256 <code>{graphSha256}</code></dd>
</dl>

<h3>Exposed parameters ({parameters.length})</h3>
{#each parameters as [name, parameter] (name)}
  <section class="item" aria-label={`Parameter ${name}`}>
    <h4>{name}</h4>
    <dl>
      {#each Object.entries(parameter) as [key, value] (key)}
        <dt>{key}</dt>
        <dd><code>{show(value)}</code></dd>
      {/each}
    </dl>
  </section>
{/each}
{#if !parameters.length}<p>None. The workflow values are used.</p>{/if}

<h3>Presets ({presets.length})</h3>
{#each presets as [name, values] (name)}
  <section class="item" aria-label={`Preset ${name}`}>
    <h4>{name}</h4>
    <dl>
      {#each Object.entries(values) as [key, value] (key)}
        <dt>{key}</dt>
        <dd><code>{show(value)}</code></dd>
      {/each}
    </dl>
  </section>
{/each}
{#if !presets.length}<p>None.</p>{/if}

<details>
  <summary>Full manifest JSON</summary>
  <pre>{formatExactJson(manifest)}</pre>
</details>

<style>
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.2rem 0.8rem;
    margin: 0;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .item {
    margin-block: 0.6rem;
    padding: 0.5rem;
    background: var(--subtle);
  }
  h4 {
    margin: 0 0 0.3rem;
  }
  pre {
    overflow: auto;
    max-height: 24rem;
  }
</style>
