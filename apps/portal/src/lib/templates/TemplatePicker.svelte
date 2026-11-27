<script module lang="ts">
  export type TemplatePickerProps = {
    name: string;
    currentText: () => string;
    insert: (text: string) => void;
    open?: boolean;
  };
</script>

<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import {
    api,
    ApiProblemError,
    type TemplateRenderRequest,
  } from '../api/client';
  import Button from '../ui/Button.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import { keepFocusAcross } from '../ui/focus';
  import Input from '../ui/Input.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import { pushToast } from '../ui/toasts';
  import {
    absent,
    entryStatus,
    filterTemplates,
    insertedText,
    isRequired,
    missingVariables,
    parseListing,
    pruneEntries,
    renderedText,
    renderProblems,
    requestVariables,
    type Entries,
    type Entry,
    type InsertMode,
    type Listing,
    type Template,
  } from './model';

  let {
    name,
    currentText,
    insert,
    open = $bindable(false),
  }: TemplatePickerProps = $props();
  const uid = $props.id();
  const templateChanged = 'urn:genaimage:problem:template-changed';
  const templateMissing = 'urn:genaimage:problem:template-not-found';
  const templateInvalid = 'urn:genaimage:problem:template-invalid';
  const renderInvalid = 'urn:genaimage:problem:template-render-invalid';

  type ListingState =
    | { state: 'loading' }
    | { state: 'failed'; problem: Problem }
    | ({ state: 'ready' } & Listing);
  type Rendered = { ref: string; sha256: string; text: string };

  let listing = $state<ListingState>({ state: 'loading' });
  let filter = $state('');
  let selected = $state<Template>();
  let entries = $state<Entries>({});
  let fieldErrors = $state<Record<string, string>>({});
  let problem = $state<Problem>();
  let notice = $state('');
  let rendered = $state<Rendered>();
  let rendering = $state(false);
  let refreshing = $state(false);
  let stale = $state(false);
  let mode = $state<InsertMode>();
  let listController: AbortController | undefined;
  let renderController: AbortController | undefined;
  let renderRun = 0;
  let listHeading = $state<HTMLElement>();
  let fillHeading = $state<HTMLElement>();
  let problemBox = $state<HTMLElement>();

  const templates = $derived(
    listing.state === 'ready' ? listing.templates : [],
  );
  const diagnostics = $derived(
    listing.state === 'ready' ? listing.diagnostics : [],
  );
  const unreadable = $derived(
    diagnostics.filter(({ path }) => path !== '').length,
  );
  const omissions = $derived(diagnostics.filter(({ path }) => path === ''));
  const visible = $derived(filterTemplates(templates, filter));
  const canInsert = $derived(
    !!selected &&
      !!rendered &&
      rendered.ref === selected.ref &&
      rendered.sha256 === selected.sha256 &&
      !!mode &&
      !rendering &&
      !refreshing,
  );
  const status = $derived(
    refreshing
      ? 'Reloading the template…'
      : rendering
        ? 'Rendering the template…'
        : rendered
          ? 'Preview ready. Insert adds exactly this text.'
          : stale
            ? 'The values changed after the last preview. Preview again before inserting.'
            : '',
  );

  onMount(() => {
    void loadListing().then(() => focusAfterUpdate(listHeading));
  });
  onDestroy(() => {
    listController?.abort();
    cancelRender();
  });

  async function focusAfterUpdate(element?: HTMLElement) {
    await tick();
    element?.focus();
  }

  const previewId = `${uid}-preview-action`;

  function fieldId(variable: string) {
    return `${uid}-${variable}`;
  }

  function textOf(variable: string) {
    const entry = entries[variable];
    return entry?.kind === 'text' ? entry.text : '';
  }

  async function loadListing() {
    listController?.abort();
    const controller = new AbortController();
    listController = controller;
    try {
      const response = await api.listTemplates({ signal: controller.signal });
      if (controller.signal.aborted) return;
      listing = { state: 'ready', ...parseListing(response) };
    } catch (failure) {
      if (controller.signal.aborted) return;
      listing = {
        state: 'failed',
        problem: problemFrom(failure, 'Could not load templates'),
      };
    }
  }

  function problemFrom(failure: unknown, title: string): Problem {
    if (failure instanceof ApiProblemError) return failure.problem;
    return {
      title,
      detail: failure instanceof Error ? failure.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  function cancelRender() {
    renderController?.abort();
    renderController = undefined;
    renderRun++;
    rendering = false;
  }

  function invalidatePreview(variable?: string) {
    stale = stale || !!rendered || rendering;
    cancelRender();
    rendered = undefined;
    if (variable !== undefined)
      fieldErrors = Object.fromEntries(
        Object.entries(fieldErrors).filter(([key]) => key !== variable),
      );
  }

  function setEntry(variable: string, entry: Entry) {
    entries = { ...entries, [variable]: entry };
    invalidatePreview(variable);
  }

  async function choose(template: Template) {
    cancelRender();
    selected = template;
    entries = {};
    fieldErrors = {};
    problem = undefined;
    notice = '';
    rendered = undefined;
    stale = false;
    mode = undefined;
    await focusAfterUpdate(fillHeading);
  }

  async function back() {
    cancelRender();
    selected = undefined;
    entries = {};
    fieldErrors = {};
    problem = undefined;
    notice = '';
    rendered = undefined;
    stale = false;
    mode = undefined;
    await focusAfterUpdate(listHeading);
  }

  /** Reloads the listing after the reviewed version is gone; the user must preview the reloaded text again. */
  async function refreshAfter(reviewed: Template, message: string) {
    const before = selected;
    cancelRender();
    rendered = undefined;
    stale = false;
    problem = undefined;
    refreshing = true;
    try {
      await loadListing();
      if (selected !== before) return;
      if (listing.state !== 'ready') {
        selected = undefined;
        notice = message;
        problem = listing.state === 'failed' ? listing.problem : undefined;
        await focusAfterUpdate(listHeading);
        return;
      }
      const next = listing.templates.find(({ ref }) => ref === reviewed.ref);
      if (!next) {
        selected = undefined;
        notice = `${message} This template is no longer usable. Choose another template.`;
        await focusAfterUpdate(listHeading);
        return;
      }
      selected = next;
      entries = pruneEntries(next, entries);
      fieldErrors = {};
      notice = `${message} Review the reloaded text, then preview it again before inserting.`;
      await focusAfterUpdate(fillHeading);
    } finally {
      refreshing = false;
    }
  }

  async function preview() {
    const template = selected;
    if (!template || rendering || refreshing) return;
    const missing = missingVariables(template, entries);
    if (missing.length) {
      fieldErrors = Object.fromEntries(
        missing.map((variable) => [
          variable,
          'Enter a value or use an empty value.',
        ]),
      );
      problem = undefined;
      await focusAfterUpdate(
        document.getElementById(fieldId(missing[0])) ?? undefined,
      );
      return;
    }
    fieldErrors = {};
    problem = undefined;
    const request: TemplateRenderRequest = {
      ref: template.ref,
      sha256: template.sha256,
      variables: requestVariables(template, entries),
    };
    const controller = new AbortController();
    renderController?.abort();
    renderController = controller;
    const run = ++renderRun;
    rendering = true;
    rendered = undefined;
    const button = document.getElementById(previewId);
    const restoreFocus = button ? keepFocusAcross(button) : undefined;
    try {
      const response = await api.renderTemplate(request, {
        signal: controller.signal,
      });
      if (run !== renderRun) return;
      rendered = {
        ref: request.ref,
        sha256: request.sha256,
        text: renderedText(response, request),
      };
      stale = false;
    } catch (failure) {
      if (run !== renderRun) return;
      await renderFailed(failure, template);
    } finally {
      if (run === renderRun) rendering = false;
    }
    await restoreFocus?.();
  }

  async function renderFailed(failure: unknown, template: Template) {
    if (failure instanceof ApiProblemError) {
      const { type } = failure;
      if (type === templateChanged) {
        await refreshAfter(
          template,
          'The template changed after you reviewed it.',
        );
        return;
      }
      if (type === templateMissing) {
        await refreshAfter(template, 'The template is no longer available.');
        return;
      }
      if (type === templateInvalid) {
        await refreshAfter(
          template,
          'The template on disk is no longer valid.',
        );
        return;
      }
      if (type === renderInvalid) {
        const mapped = renderProblems(failure.errors, template.variables);
        fieldErrors = mapped.fields;
        problem = {
          title: 'Check the values for this template',
          recovery: 'Correct the highlighted values and preview again.',
          errors: mapped.general,
        };
        const first = template.variables.find(({ name }) =>
          Object.hasOwn(mapped.fields, name),
        );
        await focusAfterUpdate(
          first
            ? (document.getElementById(fieldId(first.name)) ?? undefined)
            : problemBox,
        );
        return;
      }
    }
    problem = problemFrom(failure, 'Could not render the template');
    notice = '';
    await focusAfterUpdate(problemBox);
  }

  function insertRendered() {
    if (!canInsert || !rendered || !mode || !selected) return;
    insert(insertedText(currentText(), rendered.text, mode));
    pushToast({
      message: `Inserted “${selected.title}” into ${name}. Review the text before you submit.`,
    });
    open = false;
  }

  function scopeLabel(template: Template) {
    return template.scope === 'user'
      ? 'User template'
      : `Pack ${template.pack_id ?? ''}`.trim();
  }
</script>

<Dialog
  bind:open
  title={`Use a template for ${name}`}
  description="Templates fill named variables. Preview the exact text, then choose Insert. The field changes only when you choose Insert."
>
  <div class="picker">
    {#if selected}
      <section class="fill" aria-labelledby={`${uid}-fill`}>
        <div class="row">
          <Button onclick={back}>Back to templates</Button>
        </div>
        <h3 id={`${uid}-fill`} tabindex="-1" bind:this={fillHeading}>
          {selected.title}
        </h3>
        <p class="meta">
          <span class="badge">{scopeLabel(selected)}</span>
          <code>{selected.ref}</code>
        </p>
        {#if selected.description}<p>{selected.description}</p>{/if}
        <h4>Template text</h4>
        <pre class="raw">{selected.text}</pre>
        {#if notice}<p role="status">{notice}</p>{/if}
        <form
          class="variables"
          onsubmit={(event) => {
            event.preventDefault();
            void preview();
          }}
        >
          {#each selected.variables as variable (variable.name)}
            {@const entry = entries[variable.name] ?? absent}
            <div class="variable">
              <Input
                id={fieldId(variable.name)}
                label={`${variable.name} (${isRequired(variable) ? 'required' : 'optional'})`}
                hint={[variable.description, entryStatus(variable, entry)]
                  .filter(Boolean)
                  .join(' ')}
                error={fieldErrors[variable.name]}
                multiline
                rows={2}
                bind:value={
                  () => textOf(variable.name),
                  (text) => setEntry(variable.name, { kind: 'text', text })
                }
              />
              <div class="row">
                {#if entry.kind === 'text' && variable.default != null}
                  <Button
                    aria-label={`Use the default for ${variable.name}`}
                    onclick={() => setEntry(variable.name, absent)}
                    >Use default</Button
                  >
                {/if}
                {#if entry.kind === 'absent' || entry.text !== ''}
                  <Button
                    aria-label={`Use an empty value for ${variable.name}`}
                    onclick={() =>
                      setEntry(variable.name, { kind: 'text', text: '' })}
                    >Use empty value</Button
                  >
                {/if}
              </div>
            </div>
          {/each}
          <Button
            type="submit"
            id={previewId}
            disabled={rendering || refreshing}
            aria-busy={rendering}>Preview</Button
          >
        </form>
        <section class="preview" aria-labelledby={`${uid}-preview`}>
          <h4 id={`${uid}-preview`}>Preview</h4>
          <p role="status" aria-live="polite">{status}</p>
          {#if problem}
            <div tabindex="-1" bind:this={problemBox}>
              <ProblemDetails {problem} />
            </div>
          {/if}
          {#if rendered}
            {#if rendered.text === ''}<p>The rendered text is empty.</p>{/if}
            <pre class="rendered">{rendered.text}</pre>
          {/if}
        </section>
        <fieldset class="insert">
          <legend>Insert into {name}</legend>
          <p>
            Current text: <span class="current"
              >{currentText() || '(empty)'}</span
            >
          </p>
          <label
            ><input
              type="radio"
              name={`${uid}-mode`}
              value="replace"
              checked={mode === 'replace'}
              onchange={() => (mode = 'replace')}
            /> Replace the current text</label
          >
          <label
            ><input
              type="radio"
              name={`${uid}-mode`}
              value="append"
              checked={mode === 'append'}
              onchange={() => (mode = 'append')}
            /> Append after the current text, with no separator</label
          >
          <Button
            variant="primary"
            disabled={!canInsert}
            onclick={insertRendered}>Insert</Button
          >
        </fieldset>
      </section>
    {:else}
      <section aria-labelledby={`${uid}-list`}>
        <h3 id={`${uid}-list`} tabindex="-1" bind:this={listHeading}>
          Choose a template
        </h3>
        {#if notice}<p role="status">{notice}</p>{/if}
        {#if listing.state === 'loading'}
          <p role="status">Loading templates…</p>
        {:else if listing.state === 'failed'}
          <ProblemDetails problem={listing.problem} />
          <Button onclick={() => loadListing()}>Try again</Button>
        {:else}
          {#if unreadable}
            <details class="diagnostics">
              <summary>
                {unreadable}
                {unreadable === 1 ? 'template' : 'templates'} could not be read
              </summary>
              <ul>
                {#each diagnostics.filter(({ path }) => path !== '') as diagnostic, index (index)}
                  <li>
                    <code>{diagnostic.ref ?? diagnostic.path}</code>: {diagnostic.error}
                  </li>
                {/each}
              </ul>
            </details>
          {/if}
          {#each omissions as omission, index (index)}
            <p>{omission.error}</p>
          {/each}
          {#if templates.length}
            <Input
              type="search"
              label="Filter templates"
              hint="Matches title, ID, reference and description."
              bind:value={filter}
            />
            {#if visible.length}
              <ul class="templates">
                {#each visible as template (template.ref)}
                  <li>
                    <h4>{template.title}</h4>
                    <p class="meta">
                      <span class="badge">{scopeLabel(template)}</span>
                      <code>{template.ref}</code>
                    </p>
                    {#if template.description}<p>{template.description}</p>{/if}
                    <pre class="raw">{template.text}</pre>
                    <Button
                      variant="primary"
                      aria-label={`Use “${template.title}”`}
                      onclick={() => choose(template)}
                      >Use “{template.title}”</Button
                    >
                  </li>
                {/each}
              </ul>
            {:else}
              <p>No template matches “{filter}”.</p>
            {/if}
          {:else}
            <p>
              No templates are available. Add a template file to the templates
              directory in Settings, or use a pack that declares templates.
            </p>
          {/if}
        {/if}
      </section>
    {/if}
  </div>
</Dialog>

<style>
  .picker,
  .variables,
  .variable,
  .fill,
  .preview,
  .insert,
  .templates li {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
  }
  .templates {
    display: grid;
    gap: var(--space-3);
    padding: 0;
    list-style: none;
  }
  .templates li {
    border-block-start: 1px solid var(--border);
    padding-block-start: var(--space-2);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
    margin: 0;
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    padding: 0 0.4rem;
    font-size: var(--text-small);
  }
  .raw,
  .rendered {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 12rem;
    overflow: auto;
  }
  .rendered {
    max-height: none;
  }
  .preview p,
  .insert p,
  .fill > p {
    margin: 0;
  }
  .current {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .insert {
    border: 0;
    margin: 0;
    padding: 0;
  }
  .variables :global(.ui-field) {
    width: 100%;
  }
  .diagnostics summary {
    cursor: pointer;
  }
  p[role='status'] {
    margin: 0;
  }
</style>
