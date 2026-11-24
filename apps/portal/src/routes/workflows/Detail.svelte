<script lang="ts">
  import { api } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails from '../../lib/ui/ProblemDetails.svelte';
  import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import { workflowProblem } from './errors';

  let { params, query }: RouteProps = $props();
  const backLink = $derived.by(() => {
    const from = query.get('from');
    const filters = from === null ? '' : new URLSearchParams(from).toString();
    return `#/workflows${filters ? `?${filters}` : ''}`;
  });
  const workflowId = $derived(params.id);
  type Details = components['schemas']['WorkflowDetails'];
  type Validation = components['schemas']['WorkflowValidationResponse'];
  let workflow = $state.raw<Details>();
  let validation = $state.raw<Validation>();
  let problem = $state<Problem>();
  let validationProblem = $state<Problem>();
  let loading = $state(true);
  let validating = $state(false);

  $effect(() => {
    const id = workflowId;
    const controller = new AbortController();
    workflow = undefined;
    validation = undefined;
    problem = undefined;
    validationProblem = undefined;
    loading = true;
    validating = false;
    async function load() {
      try {
        const result = await api.getWorkflow(id, { signal: controller.signal });
        if (controller.signal.aborted) return;
        workflow = result;
        loading = false;
        validating = true;
        try {
          const checked = await api.validateWorkflow(
            id,
            {},
            { signal: controller.signal },
          );
          if (!controller.signal.aborted) validation = checked;
        } catch (error) {
          if (!controller.signal.aborted)
            validationProblem = workflowProblem(error);
        } finally {
          if (!controller.signal.aborted) validating = false;
        }
      } catch (error) {
        if (!controller.signal.aborted) {
          problem = workflowProblem(error);
          loading = false;
        }
      }
    }
    void load();
    return () => controller.abort();
  });

  const basic = $derived(
    Object.entries(workflow?.parameters ?? {}).filter(
      ([, parameter]) => parameter.advanced !== true,
    ),
  );
  const advanced = $derived(
    Object.entries(workflow?.parameters ?? {}).filter(
      ([, parameter]) => parameter.advanced === true,
    ),
  );
  const display = (value: unknown) =>
    JSON.stringify(value, (_key, item: unknown) =>
      typeof item === 'number' &&
      Number.isInteger(item) &&
      !Number.isSafeInteger(item)
        ? '[exact integer unavailable]'
        : item,
    );
  function useWorkflow(id: string) {
    window.location.hash = `#/generate?${new URLSearchParams({ workflow: id })}`;
  }
</script>

<a href={backLink}>Back to workflows</a>
{#if loading}
  <h1>Workflow detail</h1>
  <p role="status">Loading workflow…</p>
{:else if problem}
  <h1>Workflow detail</h1>
  <ProblemDetails {problem} />
{:else if workflow}
  <h1>{workflow.title || workflow.id}</h1>
  {#if workflow.description}<p>{workflow.description}</p>{/if}
  <p>
    Workflow ID: <code>{workflow.id}</code> · Version:
    <code>{workflow.version}</code>
  </p>
  <div class="action">
    <Button variant="primary" onclick={() => useWorkflow(params.id)}
      >Use this workflow</Button
    >
  </div>

  <section>
    <h2>Validation</h2>
    <p>
      Checked with manifest defaults and no overrides; required values may still
      need to be supplied.
    </p>
    {#if validating}<p role="status">Checking workflow…</p>
    {:else if validationProblem}<ProblemDetails problem={validationProblem} />
    {:else if validation}
      <p>
        {validation.valid
          ? 'Valid with these values.'
          : 'Not confirmed valid with these values.'}
      </p>
      <p>Node check: {validation.node_check.status}</p>
      {#if validation.node_check.reason}<p>
          {validation.node_check.reason}
        </p>{/if}
      {#if validation.node_check.missing_classes.length}<p>
          Missing nodes: {validation.node_check.missing_classes.join(', ')}
        </p>{/if}
      {#if validation.parameter_errors.length}<ul>
          {#each validation.parameter_errors as error, index (index)}<li>
              <strong>{error.parameter}:</strong>
              {error.reason}
            </li>{/each}
        </ul>{/if}
    {/if}
  </section>

  <section>
    <h2>Parameters</h2>
    {#if !basic.length && !advanced.length}<p>No parameters declared.</p>{/if}
    {#each [{ name: 'Basic', entries: basic }, { name: 'Advanced', entries: advanced }] as group (group.name)}
      {#if group.entries.length}
        <h3>{group.name}</h3>
        {#each group.entries as [name, parameter] (name)}
          <article class="panel">
            <h4>{name}</h4>
            {#if parameter.description}<p>{parameter.description}</p>{/if}
            <dl>
              <dt>Type</dt>
              <dd><code>{parameter.kind}</code></dd>
              <dt>Binding</dt>
              <dd>
                Node <code>{parameter.node}</code>, input
                <code>{parameter.input}</code>
              </dd>
              <dt>Required</dt>
              <dd>
                {parameter.required
                  ? 'Yes'
                  : 'No'}{parameter.declared_required === null
                  ? ' (default)'
                  : parameter.declared_required === undefined
                    ? ' (declaration unavailable)'
                    : ''}
              </dd>
              <dt>Default</dt>
              <dd>
                {#if parameter.default != null}<code
                    >{display(parameter.default)}</code
                  >{:else}Not specified or null{/if}
              </dd>
              {#if parameter.widget != null}<dt>UI hint</dt>
                <dd><code>{display(parameter.widget)}</code></dd>{/if}
              {#if parameter.group != null}<dt>Group</dt>
                <dd><code>{display(parameter.group)}</code></dd>{/if}
              {#if parameter.minimum != null}<dt>Minimum</dt>
                <dd><code>{display(parameter.minimum)}</code></dd>{/if}
              {#if parameter.maximum != null}<dt>Maximum</dt>
                <dd><code>{display(parameter.maximum)}</code></dd>{/if}
              {#if parameter.allowed_values != null}<dt>Allowed values</dt>
                <dd><code>{display(parameter.allowed_values)}</code></dd>{/if}
              {#if parameter.step != null}<dt>Suggested step</dt>
                <dd><code>{display(parameter.step)}</code></dd>{/if}
              {#if parameter.options != null}<dt>Suggested options</dt>
                <dd><code>{display(parameter.options)}</code></dd>{/if}
              {#if parameter.placeholder != null}<dt>Placeholder</dt>
                <dd><code>{display(parameter.placeholder)}</code></dd>{/if}
            </dl>
          </article>
        {/each}
      {/if}
    {/each}
  </section>
  <section>
    <h2>Presets</h2>
    {#if !Object.keys(workflow.presets).length}<p>No presets declared.</p>{/if}
    {#each Object.entries(workflow.presets) as [name, values] (name)}
      <article class="panel">
        <h3>{name}</h3>
        {#if Object.keys(values).length}<dl>
            {#each Object.entries(values) as [key, value] (key)}<dt>{key}</dt>
              <dd><code>{display(value)}</code></dd>{/each}
          </dl>{:else}<p>No values declared.</p>{/if}
      </article>
    {/each}
  </section>
  <section>
    <h2>Declared outputs</h2>
    {#if workflow.outputs.length}<ul>
        {#each workflow.outputs as output, index (index)}<li>
            Node <code>{output.node}</code> · Type
            <code>{output.kind}</code>{output.declared_kind === null
              ? ' (default)'
              : output.declared_kind === undefined
                ? ' (declaration unavailable)'
                : ''}
          </li>{/each}
      </ul>{:else}<p>No outputs declared.</p>{/if}
  </section>
{/if}

<style>
  h1 {
    margin-block: var(--space-3);
  }
  section {
    margin-block-start: var(--space-5);
  }
  section > h3,
  section > p,
  section > .panel {
    margin-block-start: var(--space-3);
  }
  .panel {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: var(--space-3);
  }
  .panel h3,
  .panel h4 {
    margin: 0 0 var(--space-2);
  }
  dl {
    display: grid;
    grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr);
    gap: var(--space-1) var(--space-3);
  }
  dt {
    font-weight: var(--weight-strong);
  }
  dd {
    margin: 0;
  }
  code {
    white-space: pre-wrap;
  }
  .action {
    margin-block-start: var(--space-3);
  }
</style>
