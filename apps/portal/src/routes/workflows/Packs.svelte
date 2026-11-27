<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { keepFocusAcross } from '../../lib/ui/focus';
  import { SvelteMap } from 'svelte/reactivity';
  import { api } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import { workflowProblem } from './errors';
  import { validationView } from './model';
  import type { ValidationView } from './model';

  let { generation = 0 }: { generation?: number } = $props();
  type Pack = components['schemas']['PackSummary'];
  type Diagnostic = components['schemas']['WorkflowDiagnostic'];
  type Check =
    | { state: 'checking' }
    | { state: 'done'; view: ValidationView }
    | { state: 'failed'; problem: Problem };

  let packs = $state.raw<Pack[]>([]);
  let diagnostics = $state.raw<Diagnostic[]>([]);
  let loading = $state(true);
  let problem = $state<Problem>();
  const checks = new SvelteMap<string, Check>();
  const checkControllers = new SvelteMap<string, AbortController>();
  let loadSeq = 0;
  const lifetime = new AbortController();
  onDestroy(() => {
    lifetime.abort();
    abortChecks();
  });

  function abortChecks() {
    for (const controller of checkControllers.values()) controller.abort();
    checkControllers.clear();
    checks.clear();
  }

  async function loadPacks() {
    const seq = ++loadSeq;
    loading = true;
    problem = undefined;
    abortChecks();
    try {
      const listing = await api.listPacks({ signal: lifetime.signal });
      if (seq !== loadSeq) return;
      packs = listing.packs ?? [];
      diagnostics = listing.diagnostics ?? [];
    } catch (error) {
      if (seq === loadSeq && !lifetime.signal.aborted)
        problem = workflowProblem(error, 'Could not load packs');
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  $effect(() => {
    void generation;
    untrack(() => {
      void loadPacks();
    });
  });

  async function checkRequirements(id: string, button: HTMLButtonElement) {
    checkControllers.get(id)?.abort();
    const controller = new AbortController();
    checkControllers.set(id, controller);
    const restoreFocus = keepFocusAcross(button);
    checks.set(id, { state: 'checking' });
    try {
      const response = await api.validatePack(id, {
        signal: controller.signal,
      });
      if (!controller.signal.aborted)
        checks.set(id, { state: 'done', view: validationView(response) });
    } catch (error) {
      if (!controller.signal.aborted)
        checks.set(id, {
          state: 'failed',
          problem: workflowProblem(error, 'Could not check requirements'),
        });
    } finally {
      if (checkControllers.get(id) === controller) checkControllers.delete(id);
      await restoreFocus();
    }
  }

  const modelLines = (pack: Pack) =>
    Object.entries(pack.requires?.models ?? {}).map(
      ([folder, names]) => `${folder}: ${(names ?? []).join(', ')}`,
    );
</script>

<section class="packs" aria-labelledby="packs-heading">
  <h2 id="packs-heading">Packs</h2>
  <p>
    Packs declare the node classes and models their workflows need. GenAImage
    never installs anything: install missing models and nodes in ComfyUI
    yourself, then check again.
  </p>
  {#if loading}
    <p role="status">Loading packs…</p>
  {:else if problem}
    <ProblemDetails {problem} />
  {:else}
    {#if diagnostics.length}
      <section class="problems" aria-label="Pack discovery problems">
        <p>Some packs could not be loaded. Check their pack.toml files:</p>
        <ul>
          {#each diagnostics as diagnostic, index (index)}<li>
              <strong>{diagnostic.path}:</strong>
              {diagnostic.error}
            </li>{/each}
        </ul>
      </section>
    {/if}
    {#if packs.length === 0}
      <p>
        No packs found. Add a pack directory, containing a pack.toml, to the
        configured packs path.
      </p>
    {:else}
      <ul class="pack-list">
        {#each packs as pack, index (index)}
          {@const check = checks.get(pack.id)}
          <li class="pack">
            <h3>
              {pack.id}
              <span class="version">version {pack.version}</span>
            </h3>
            {#if pack.description}<p>{pack.description}</p>{/if}
            <dl>
              <dt>Workflows</dt>
              <dd>
                {(pack.workflow_ids ?? []).length}
                {#if pack.workflow_ids?.length}
                  (<code>{pack.workflow_ids.join(', ')}</code>)
                {/if}
              </dd>
              <dt>Node classes</dt>
              <dd>
                {pack.requires?.node_classes?.length
                  ? pack.requires.node_classes.join(', ')
                  : 'None declared'}
              </dd>
              <dt>Models</dt>
              <dd>
                {#if modelLines(pack).length}
                  <ul>
                    {#each modelLines(pack) as line, lineIndex (lineIndex)}<li>
                        {line}
                      </li>{/each}
                  </ul>
                {:else}None declared{/if}
              </dd>
              <dt>Templates</dt>
              <dd>{(pack.templates ?? []).length} listed</dd>
            </dl>
            {#if pack.diagnostics?.length}
              <ul class="problems" aria-label={`Problems in ${pack.id}`}>
                {#each pack.diagnostics as line, lineIndex (lineIndex)}<li>
                    {line}
                  </li>{/each}
              </ul>
            {/if}
            <Button
              aria-label={`Check requirements for ${pack.id}`}
              disabled={check?.state === 'checking'}
              onclick={(event) =>
                checkRequirements(pack.id, event.currentTarget)}
            >
              Check requirements
            </Button>
            <div class="result" aria-live="polite">
              {#if check?.state === 'checking'}
                <p>Checking requirements…</p>
              {:else if check?.state === 'failed'}
                <ProblemDetails problem={check.problem} />
              {:else if check?.state === 'done'}
                <section class={['verdict', `verdict-${check.view.verdict}`]}>
                  <h4>{check.view.title}</h4>
                  <p>{check.view.headline}</p>
                  {#if check.view.reason}<p>{check.view.reason}</p>{/if}
                  {#if check.view.nodes.length}
                    <h5>Node classes</h5>
                    <ul>
                      {#each check.view.nodes as node, nodeIndex (nodeIndex)}
                        <li>{node.name}: {node.label}</li>
                      {/each}
                    </ul>
                  {/if}
                  {#if check.view.models.length}
                    <h5>Models</h5>
                    <ul>
                      {#each check.view.models as model, modelIndex (modelIndex)}
                        <li>
                          {model.folder}: {model.name}, {model.label}{model.loader
                            ? ` (loader ${model.loader})`
                            : ''}{model.reason ? `. ${model.reason}` : ''}
                        </li>
                      {/each}
                    </ul>
                  {/if}
                  {#if check.view.diagnostics.length}
                    <h5>Problems</h5>
                    <ul>
                      {#each check.view.diagnostics as line, lineIndex (lineIndex)}
                        <li>{line}</li>
                      {/each}
                    </ul>
                  {/if}
                </section>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .packs {
    margin-block-start: var(--space-6);
  }
  .pack-list {
    display: grid;
    gap: var(--space-4);
    list-style: none;
    padding: 0;
  }
  .pack {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
    display: grid;
    gap: var(--space-2);
    justify-items: start;
  }
  .pack > * {
    max-width: 100%;
  }
  .pack h3 {
    margin: 0;
  }
  .version {
    font-size: var(--text-small);
    color: var(--muted);
    font-weight: normal;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr);
    gap: var(--space-1) var(--space-3);
    margin: 0;
    width: 100%;
  }
  dt {
    font-weight: var(--weight-strong);
  }
  dd {
    margin: 0;
  }
  dd ul {
    margin: 0;
    padding-inline-start: var(--space-4);
  }
  .problems {
    color: var(--danger);
  }
  .verdict {
    border: 1px solid var(--border);
    border-inline-start-width: 4px;
    border-radius: var(--radius-control);
    padding: var(--space-3);
    width: 100%;
  }
  .verdict-missing,
  .verdict-unknown {
    border-inline-start-color: var(--danger);
  }
  dd,
  .pack li,
  .verdict li {
    overflow-wrap: anywhere;
  }
  .verdict h4,
  .verdict h5 {
    margin: var(--space-2) 0 var(--space-1);
  }
</style>
