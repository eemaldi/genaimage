<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { Workflow } from '../generate/model';
  import type { PendingSubmission } from '../generate/pending';
  import type { SubmissionControls } from '../generate/Submission.svelte';
  import {
    checkpointParameter,
    defaultTestPrompt,
    parametersOf,
    showValue,
    signatureOf,
    testRows,
  } from './test-run';

  type Inherited = { parameter: string; description: string };
  type Reviewed = {
    snapshot: PendingSubmission;
    signature: string;
    inherited: Inherited[];
  };
  let {
    workflow,
    checkpoint,
    controls,
    reloadWorkflow,
  }: {
    workflow: Workflow;
    checkpoint: string;
    controls: SubmissionControls;
    reloadWorkflow: (problem: Problem) => void;
  } = $props();

  let prompt = $state(defaultTestPrompt);
  let reviewed = $state.raw<Reviewed>();
  let problem = $state<Problem>();
  let validating = $state(false);
  let heading = $state<HTMLElement>();
  let validation: AbortController | undefined;
  onDestroy(() => validation?.abort());

  let plan = $derived(testRows(workflow, checkpoint, prompt));
  let signature = $derived(signatureOf(plan.rows));
  let current = $derived(
    reviewed?.signature === signature &&
      reviewed.snapshot.body.workflow_revision === workflow.revision
      ? reviewed
      : undefined,
  );
  let restoredCheckpoint = $derived.by(() => {
    const value = (
      controls.pending?.body.parameters as Record<string, unknown> | undefined
    )?.[checkpointParameter];
    return typeof value === 'string' ? value : undefined;
  });
  let locked = $derived(controls.editingLocked || controls.accepted);

  function describe(failure: unknown, title: string): Problem {
    return failure instanceof ApiProblemError
      ? failure.problem
      : {
          title,
          detail: failure instanceof Error ? failure.message : undefined,
        };
  }

  async function review() {
    if (validating || locked || plan.blockers.length) return;
    const revision = workflow.revision;
    if (!revision) {
      problem = {
        title: 'The server did not provide a workflow revision',
        recovery: 'Check again to reload the workflow.',
      };
      return;
    }
    const parameters = parametersOf(plan.rows);
    const rowsSignature = signature;
    const controller = new AbortController();
    validation?.abort();
    validation = controller;
    validating = true;
    problem = undefined;
    reviewed = undefined;
    try {
      const result = await api.validateWorkflow(
        workflow.id,
        { parameters, workflow_revision: revision },
        { exactNumbers: true, signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      if (result.revision !== revision) {
        reloadWorkflow({
          title: 'The workflow changed during validation',
          recovery: 'Review the reloaded workflow before running.',
        });
        return;
      }
      const errors = result.parameter_errors.map(
        ({ parameter, reason }) => `${parameter}: ${reason}`,
      );
      if (
        !result.valid ||
        errors.length ||
        result.node_check.status !== 'available'
      ) {
        const node = result.node_check;
        problem = {
          title: 'The workflow did not pass server validation',
          detail:
            node.status === 'missing'
              ? `Missing backend nodes: ${node.missing_classes.join(', ')}.`
              : errors.join(' ') || (node.reason ?? undefined),
          recovery:
            'Install the missing ComfyUI nodes or correct the value, then review again. Nothing was run.',
        };
        return;
      }
      const inherited = (result.inherited_inputs ?? [])
        .filter(({ parameter }) => !Object.hasOwn(parameters, parameter))
        .map(({ parameter, source }) => ({
          parameter,
          description:
            source.kind === 'connection'
              ? `connected to node ${source.node} output ${source.output}`
              : source.json,
        }));
      reviewed = {
        snapshot: {
          body: Object.freeze({
            workflow: workflow.id,
            parameters,
            workflow_revision: revision,
          }),
          version: workflow.version,
          key: crypto.randomUUID(),
        },
        signature: rowsSignature,
        inherited,
      };
      await tick();
      heading?.focus();
    } catch (failure) {
      if (controller.signal.aborted) return;
      problem = describe(failure, 'Could not validate the test generation');
    } finally {
      if (validation === controller) {
        validating = false;
        validation = undefined;
      }
    }
  }

  function run() {
    if (!current || locked) return;
    controls.submit(current.snapshot, (conflict) => {
      reviewed = undefined;
      if (conflict) reloadWorkflow(conflict);
    });
  }
</script>

<h3>Test generation values</h3>
<p>
  Review everything below before running. Nothing is substituted: the checkpoint
  is your choice and the other values come from revision
  <code>{workflow.revision ?? 'unknown'}</code> of the
  <code>{workflow.id}</code> workflow.
</p>
<Input
  label="Test prompt"
  hint="Editing the prompt discards the review below."
  multiline
  rows={2}
  bind:value={prompt}
  disabled={locked}
/>
<table class="setup-table">
  <caption class="visually-hidden">Values that will be submitted</caption>
  <thead><tr><th>Parameter</th><th>Value</th><th>Origin</th></tr></thead>
  <tbody>
    {#each plan.rows as row (row.name)}
      <tr>
        <th scope="row">{row.name}</th>
        <td><code>{showValue(row.value)}</code></td>
        <td>{row.origin}</td>
      </tr>
    {/each}
  </tbody>
</table>
{#each plan.blockers as blocker (blocker)}
  <p class="setup-problem" role="alert">{blocker}</p>
{/each}
{#if problem}<ProblemDetails {problem} />{/if}
{#if validating}<p role="status">Validating with the server…</p>{/if}
{#if restoredCheckpoint !== undefined && restoredCheckpoint !== checkpoint}
  <p class="setup-problem" role="alert" data-restored-mismatch>
    The pending test submission above uses checkpoint
    <code>{restoredCheckpoint}</code>, not your current choice
    <code>{checkpoint}</code>. Retrying sends it unchanged with the same key;
    the step will not count as done unless the finished run used your current
    choice.
  </p>
{/if}

{#if current}
  <section aria-labelledby="test-review-heading">
    <h3 id="test-review-heading" tabindex="-1" bind:this={heading}>
      Reviewed test generation
    </h3>
    <p>
      Pinned to workflow revision
      <code>{current.snapshot.body.workflow_revision}</code>. The server
      validated these exact values.
    </p>
    {#if current.inherited.length}
      <p>Inherited workflow inputs that were not set here:</p>
      <ul>
        {#each current.inherited as item (item.parameter)}
          <li><code>{item.parameter}</code>: {item.description}</li>
        {/each}
      </ul>
    {/if}
    <p>
      Running sends one request with a single idempotency key. If the outcome is
      uncertain you can only retry with the same key or check Jobs.
    </p>
    <Button variant="primary" disabled={locked} onclick={run}
      >Run test generation</Button
    >
  </section>
{:else}
  <Button
    variant="primary"
    disabled={locked || validating || plan.blockers.length > 0}
    onclick={review}>Validate and review</Button
  >
{/if}
