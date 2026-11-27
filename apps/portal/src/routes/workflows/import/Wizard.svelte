<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    api,
    ApiProblemError,
    parseBindingJson,
    supportsExactNumberWrites,
    type WorkflowImportConfirmRequest,
  } from '../../../lib/api/client';
  import { setNavigationGuard } from '../../../navigation';
  import Button from '../../../lib/ui/Button.svelte';
  import { keepFocusAcross } from '../../../lib/ui/focus';
  import ProblemDetails, {
    type Problem,
  } from '../../../lib/ui/ProblemDetails.svelte';
  import { workflowHref } from '../../generate/model';
  import FileStep from './FileStep.svelte';
  import PresetsStep from './PresetsStep.svelte';
  import PreviewStep from './PreviewStep.svelte';
  import ReviewStep from './ReviewStep.svelte';
  import SaveStep from './SaveStep.svelte';
  import {
    buildManifest,
    draftFromProposal,
    fileStem,
    hasErrors,
    isProblemType,
    locateError,
    maxGraphBytes,
    parseProposal,
    problemOf,
    problemTypes,
    steps,
    validateDraft,
    type Confirmed,
    type Imported,
    type Json,
    type ManifestDraft,
    type Origin,
    type ProblemWithExtras,
    type StepId,
  } from './model';

  type Attempt = { manifest: Json; body: WorkflowImportConfirmRequest };

  let step = $state<StepId>('file');
  let imported = $state.raw<Imported>();
  let draft = $state<ManifestDraft>();
  let origin: Origin | undefined;
  let importing = $state(false);
  let importProblem = $state<Problem>();
  let selectedName = $state<string>();
  let showErrors = $state(false);
  let blocked = $state(false);
  let serverErrors = $state<Record<string, string>>({});
  let attempt = $state.raw<Attempt>();
  let saving = $state(false);
  let saveProblem = $state<ProblemWithExtras>();
  let result = $state.raw<Confirmed>();
  let writtenButUnavailable = $state<ProblemWithExtras>();
  let heading = $state<HTMLElement>();
  let importSeq = 0;
  let importController: AbortController | undefined;

  let index = $derived(steps.findIndex((item) => item.id === step));
  let errors = $derived(draft ? validateDraft(draft) : undefined);
  let finished = $derived(!!result || !!writtenButUnavailable);
  let dirty = $derived(!!imported && !finished);
  let manifestForPreview = $derived.by(() => {
    if (!draft || !origin || !errors || hasErrors(errors)) return;
    return buildManifest(draft, origin);
  });
  let suggestedId = $derived(
    isProblemType(saveProblem, problemTypes.exists) &&
      typeof saveProblem?.suggested_id === 'string'
      ? saveProblem.suggested_id
      : undefined,
  );
  let mismatch = $derived(
    isProblemType(saveProblem, problemTypes.graphMismatch),
  );
  let uncertain = $derived(
    !!saveProblem &&
      !saveProblem.status &&
      !isProblemType(saveProblem, problemTypes.exists),
  );
  let mappedErrors = $derived(
    draft
      ? (saveProblem?.errors ?? []).map((item) => ({
          ...item,
          ...locateError(item.key, draft!),
        }))
      : [],
  );

  async function go(next: StepId) {
    step = next;
    blocked = false;
    await tick();
    heading?.focus();
  }

  function fail(title: string, detail: string, recovery: string) {
    importProblem = { title, detail, recovery };
  }

  async function choose(file: File) {
    if (
      imported &&
      !finished &&
      !window.confirm(
        'Replace the imported workflow? Your edits to the current one are discarded.',
      )
    )
      return;
    importController?.abort();
    const controller = (importController = new AbortController());
    const seq = ++importSeq;
    importProblem = undefined;
    selectedName = file.name;
    if (file.size > maxGraphBytes) {
      return fail(
        'The file is too large',
        `${file.name} is ${file.size} bytes; the limit is ${maxGraphBytes} bytes (2 MiB).`,
        'Export the workflow again without extra data, or remove unused nodes.',
      );
    }
    if (!supportsExactNumberWrites()) {
      return fail(
        'This browser cannot keep numbers exact',
        'Importing would round large seeds or rewrite values such as 2.0.',
        'Use a current Chromium, Firefox or Safari release.',
      );
    }
    importing = true;
    try {
      const text = await file.text();
      if (seq !== importSeq) return;
      let graph: unknown;
      try {
        graph = parseBindingJson(text);
      } catch (error) {
        if (error instanceof SyntaxError)
          return fail(
            'The file is not valid JSON',
            error.message,
            'Export the workflow again with Workflow, Export (API) in ComfyUI.',
          );
        throw error;
      }
      if (!graph || typeof graph !== 'object' || Array.isArray(graph))
        return fail(
          'The file is not a workflow graph',
          'An API-format export is a JSON object keyed by node ID.',
          'Export the workflow with Workflow, Export (API) in ComfyUI.',
        );
      const name = fileStem(file.name);
      const body = {
        graph: graph as Record<string, never>,
        ...(name ? { name } : {}),
      };
      const response = parseProposal(
        await api.importWorkflow(body, { signal: controller.signal }),
      );
      if (seq !== importSeq) return;
      const next = draftFromProposal(response);
      imported = {
        graph: graph as Json,
        proposal: response,
        fileName: file.name,
      };
      draft = next.draft;
      origin = next.origin;
      attempt = undefined;
      saveProblem = undefined;
      serverErrors = {};
      showErrors = false;
      await go('review');
    } catch (error) {
      if (seq !== importSeq || controller.signal.aborted) return;
      importProblem = problemOf(error, 'The file could not be analysed');
    } finally {
      if (seq === importSeq) importing = false;
    }
  }

  function next() {
    if (!draft || !errors) return;
    if (step === 'review' || step === 'presets') {
      if (hasErrors(errors)) {
        showErrors = true;
        blocked = true;
        return;
      }
    }
    const target = steps[index + 1]?.id;
    if (!target) return;
    if (target === 'save') attempt = freeze();
    void go(target);
  }

  function freeze(): Attempt | undefined {
    if (!imported || !draft || !origin) return;
    const manifest = buildManifest(draft, origin);
    return {
      manifest,
      body: {
        graph: imported.graph as Record<string, never>,
        graph_sha256: imported.proposal.graph_sha256,
        manifest: manifest as WorkflowImportConfirmRequest['manifest'],
      },
    };
  }

  async function save(button?: HTMLElement) {
    if (!attempt || saving || finished) return;
    if (!supportsExactNumberWrites()) {
      saveProblem = {
        title: 'This browser cannot keep numbers exact',
        detail:
          'Saving would round large numbers or rewrite values such as 2.0.',
        recovery: 'Use a current Chromium, Firefox or Safari release.',
      };
      return;
    }
    const restoreFocus = button ? keepFocusAcross(button) : undefined;
    saving = true;
    saveProblem = undefined;
    serverErrors = {};
    try {
      const response = (await api.confirmWorkflowImport(attempt.body)) as
        Partial<Confirmed> | undefined;
      result = {
        id: response?.id ?? String(attempt.manifest.id),
        revision: response?.revision ?? '',
        manifest_path: response?.manifest_path ?? '',
      };
      await tick();
      heading?.focus();
    } catch (error) {
      if (
        error instanceof ApiProblemError &&
        isProblemType(error.problem, problemTypes.ambiguous)
      ) {
        writtenButUnavailable = error.problem as ProblemWithExtras;
        await tick();
        heading?.focus();
        return;
      }
      saveProblem = problemOf(error, 'The workflow could not be saved');
      if (isProblemType(saveProblem, problemTypes.manifestInvalid)) {
        serverErrors = Object.fromEntries(
          (saveProblem.errors ?? []).map(({ key, reason }) => [key, reason]),
        );
      }
    } finally {
      saving = false;
      await restoreFocus?.();
    }
  }

  function useSuggested(id: string, button: HTMLElement) {
    if (!draft || saving) return;
    draft.id = id;
    attempt = freeze();
    saveProblem = undefined;
    void save(button);
  }

  function reset() {
    importSeq++;
    importController?.abort();
    imported = undefined;
    draft = undefined;
    origin = undefined;
    attempt = undefined;
    result = undefined;
    writtenButUnavailable = undefined;
    saveProblem = undefined;
    serverErrors = {};
    importProblem = undefined;
    selectedName = undefined;
    showErrors = false;
    void go('file');
  }

  onMount(() => {
    function beforeUnload(event: BeforeUnloadEvent) {
      if (dirty || saving) {
        event.preventDefault();
        event.returnValue = '';
      }
    }
    setNavigationGuard(() => {
      if (saving) {
        window.alert(
          'A save is in progress. Wait for its result before leaving.',
        );
        return false;
      }
      return (
        !dirty ||
        window.confirm(
          'The imported workflow has not been saved. Leave and discard it?',
        )
      );
    });
    window.addEventListener('beforeunload', beforeUnload);
    return () => {
      importSeq++;
      importController?.abort();
      window.removeEventListener('beforeunload', beforeUnload);
      setNavigationGuard();
    };
  });
</script>

<h1>Import a workflow</h1>
<p>
  Turn a ComfyUI API-format export into a GenAImage workflow. You review every
  value, and nothing is saved until the last step.
</p>
<p><a href="#/workflows">Back to workflows</a></p>

<nav aria-label="Import steps">
  <ol class="steps">
    {#each steps as item, at (item.id)}
      <li aria-current={item.id === step ? 'step' : undefined}>
        {at + 1}. {item.label}
      </li>
    {/each}
  </ol>
</nav>
<div class="visually-hidden" role="status" aria-live="polite">
  Step {index + 1} of {steps.length}: {steps[index].label}
</div>

<section aria-labelledby="import-step-heading">
  <h2 id="import-step-heading" tabindex="-1" bind:this={heading}>
    {#if result}
      Workflow saved
    {:else if writtenButUnavailable}
      Workflow written but unavailable
    {:else}
      {index + 1}. {steps[index].label}
    {/if}
  </h2>

  {#if step === 'file'}
    <FileStep fileName={selectedName} {choose} />
    <p role="status" aria-live="polite">
      {importing ? 'Analysing the workflow…' : ''}
    </p>
    {#if importProblem}<ProblemDetails problem={importProblem} />{/if}
    {#if imported && draft}
      <p>
        <Button variant="primary" onclick={() => go('review')}
          >Continue with {imported.fileName}</Button
        >
      </p>
    {/if}
  {:else if draft && imported && errors}
    {#if step === 'review'}
      <ReviewStep
        bind:draft
        proposal={imported.proposal}
        {errors}
        {serverErrors}
        {showErrors}
      />
    {:else if step === 'presets'}
      <PresetsStep bind:draft {errors} {serverErrors} {showErrors} />
    {:else if step === 'preview' && manifestForPreview}
      <PreviewStep manifest={manifestForPreview} />
    {:else if step === 'save' && attempt}
      {#if result}
        <p role="status">
          The workflow <code>{result.id}</code> was written and is ready to use.
        </p>
        <p>
          <a class="ui-button primary" href={workflowHref(result.id)}
            >Generate with this workflow</a
          >
          <a
            class="ui-button"
            href={`#/workflows/${encodeURIComponent(result.id)}`}
            >View workflow details</a
          >
          <Button onclick={reset}>Import another workflow</Button>
        </p>
      {:else if writtenButUnavailable}
        <div role="alert" class="ui-problem">
          <p class="ui-problem-title">
            The workflow was written, but it is not available yet.
          </p>
          <p>
            {writtenButUnavailable.detail} The files for
            <code
              >{String(writtenButUnavailable.id ?? attempt.manifest.id)}</code
            >
            are in
            <code>{String(writtenButUnavailable.manifest_path ?? '')}</code>
            and are not rolled back.
          </p>
          <p>
            <strong>What to do next:</strong>
            {writtenButUnavailable.recovery} Another workflow declares the same ID.
            Remove or rename the duplicate, then refresh the workflows list. Saving
            again would not create anything new.
          </p>
        </div>
        <p><a class="ui-button" href="#/workflows">Go to workflows</a></p>
      {:else}
        <SaveStep
          manifest={attempt.manifest}
          graphSha256={attempt.body.graph_sha256}
          nodeCount={Object.keys(imported.graph).length}
        />
      {/if}

      {#if saveProblem}
        <ProblemDetails problem={{ ...saveProblem, errors: undefined }} />
        {#if mappedErrors.length}
          <ul class="mapped">
            {#each mappedErrors as item, at (at)}
              <li>
                <strong>{item.label}:</strong>
                {item.reason}
                {#if item.step !== 'save'}
                  <Button onclick={() => go(item.step)}
                    >Fix in step {steps.findIndex((s) => s.id === item.step) +
                      1}</Button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
        {#if suggestedId}
          <p>
            <Button
              variant="primary"
              disabled={saving}
              onclick={(event) =>
                useSuggested(suggestedId, event.currentTarget)}
              >Use {suggestedId} and save</Button
            >
          </p>
        {/if}
        {#if mismatch}
          <p>
            The graph no longer matches the analysed one, so nothing was
            written. Choose the file again to start a new import.
            <Button onclick={reset}>Choose the file again</Button>
          </p>
        {/if}
      {/if}
    {/if}
  {/if}
</section>

{#if blocked}
  <p role="alert">
    Some fields need attention. Fix the highlighted fields to continue.
  </p>
{/if}

{#if step !== 'file' && draft && !finished}
  <p class="actions">
    <Button
      disabled={saving}
      onclick={() => go(steps[Math.max(0, index - 1)].id)}>Back</Button
    >
    {#if step === 'save'}
      {#if !suggestedId && !mismatch}
        <Button
          variant="primary"
          disabled={saving || !attempt}
          onclick={(event) => save(event.currentTarget)}
          >{saving
            ? 'Saving…'
            : uncertain
              ? 'Send the same save again'
              : 'Save workflow'}</Button
        >
      {/if}
    {:else}
      <Button variant="primary" onclick={next}>Continue</Button>
    {/if}
  </p>
{/if}

<style>
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 1rem;
    padding: 0;
    list-style: none;
  }
  .steps [aria-current='step'] {
    font-weight: 700;
  }
  .actions {
    display: flex;
    gap: 0.6rem;
  }
  .mapped {
    padding-inline-start: 1.2rem;
  }
</style>
