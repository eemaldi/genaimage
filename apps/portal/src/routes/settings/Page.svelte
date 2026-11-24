<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Button from '../../lib/ui/Button.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import { setNavigationGuard } from '../../navigation';
  import {
    display,
    formFrom,
    patchFrom,
    sections,
    validate,
    type Form,
    type Settings,
  } from './form';

  let { params: _params, query: _query }: RouteProps = $props();
  let settings = $state<Settings>();
  let form = $state<Form>({});
  let problem = $state<Problem>();
  let probeProblem = $state<Problem>();
  let probeService = $state<'comfyui' | 'llm'>();
  let etag = $state('');
  let saving = $state(false);
  let conflict = $state(false);
  let draft = $state<Form>();
  let previous = $state<Form>();
  let fieldErrors = $state<Record<string, string>>({});
  let busy = $state(false);
  let loading = $state(true);
  let notice = $state('');
  let result = $state<components['schemas']['SettingsTestResponse']>();
  let validation = $derived(settings ? validate(form, settings) : {});
  let dirty = $derived(
    settings
      ? Object.keys(form).some(
          (key) => form[key] !== display(settings!.settings[key].value),
        )
      : false,
  );
  let invalid = $derived(Object.keys(validation).length > 0);

  function showError(error: unknown) {
    problem =
      error instanceof ApiProblemError
        ? error.problem
        : {
            title: 'Request failed',
            detail:
              error instanceof Error
                ? error.message
                : 'An unexpected error occurred.',
            recovery: 'Check the local server and try again.',
          };
    fieldErrors = Object.fromEntries(
      (problem.errors ?? []).map(({ key, reason }) => [key, reason]),
    );
  }
  function update(key: string, value: string) {
    form[key] = value;
    fieldErrors = {};
    problem = undefined;
    notice = '';
    result = undefined;
    probeProblem = undefined;
  }
  function load(snapshot: { settings: Settings; etag: string }) {
    settings = snapshot.settings;
    etag = snapshot.etag;
    form = formFrom(snapshot.settings);
    fieldErrors = {};
  }
  onMount(() => {
    let mounted = true;
    api
      .getSettingsWithRevision()
      .then((response) => {
        if (mounted) load(response);
      })
      .catch((error: unknown) => {
        if (mounted) showError(error);
      })
      .finally(() => {
        if (mounted) loading = false;
      });
    function beforeUnload(event: BeforeUnloadEvent) {
      if (dirty || saving || conflict) {
        event.preventDefault();
        event.returnValue = '';
      }
    }
    setNavigationGuard(() => {
      if (saving) {
        window.alert(
          'A save is in progress. Wait for its result before leaving settings.',
        );
        return false;
      }
      return (
        !(dirty || conflict) ||
        window.confirm(
          'You have unsaved changes. Leave settings and discard them?',
        )
      );
    });
    window.addEventListener('beforeunload', beforeUnload);
    return () => {
      mounted = false;
      window.removeEventListener('beforeunload', beforeUnload);
      setNavigationGuard();
    };
  });
  async function testConnection(service: 'comfyui' | 'llm') {
    if (
      !settings ||
      busy ||
      conflict ||
      Object.keys(validation).some((key) => key.startsWith(`${service}.`))
    )
      return;
    busy = true;
    probeService = service;
    probeProblem = undefined;
    result = undefined;
    try {
      result = await api.testSettings(patchFrom(form, settings, service), {
        service,
      });
    } catch (error) {
      probeProblem =
        error instanceof ApiProblemError
          ? error.problem
          : {
              title: 'Connection test failed',
              detail:
                error instanceof Error
                  ? error.message
                  : 'An unexpected error occurred.',
              recovery: 'Check the local server and try again.',
            };
    } finally {
      busy = false;
    }
  }
  async function reloadConflict() {
    if (!settings || busy) return;
    busy = true;
    try {
      const pending = { ...form };
      const baseline = formFrom(settings);
      const fresh = await api.getSettingsWithRevision();
      load(fresh);
      draft = pending;
      previous = baseline;
      problem = undefined;
      notice =
        'Review the current and proposed values, then choose whether to re-apply your changes.';
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }
  function reapply() {
    if (!draft || !previous || !settings) return;
    const skipped: string[] = [];
    for (const key of Object.keys(draft)) {
      if (draft[key] === previous[key]) continue;
      if (!settings.settings[key] || settings.settings[key].read_only)
        skipped.push(key);
      else form[key] = draft[key];
    }
    draft = undefined;
    previous = undefined;
    conflict = false;
    notice = skipped.length
      ? `Could not re-apply read-only settings: ${skipped.join(', ')}.`
      : 'Changes re-applied. Review them before saving.';
  }
  async function save() {
    if (
      !settings ||
      invalid ||
      !dirty ||
      busy ||
      conflict ||
      Object.keys(fieldErrors).length
    )
      return;
    busy = true;
    saving = true;
    problem = undefined;
    fieldErrors = {};
    notice = '';
    try {
      const saved = await api.putSettingsWithRevision(
        patchFrom(form, settings),
        etag,
      );
      load(saved);
      try {
        load(await api.getSettingsWithRevision());
        notice = 'Settings saved and refreshed.';
      } catch (error) {
        notice =
          'Settings were saved, but could not be refreshed. Reload the page to verify them.';
        showError(error);
      }
    } catch (error) {
      showError(error);
      if (error instanceof ApiProblemError && error.status === 412)
        conflict = true;
      if (!(error instanceof ApiProblemError) || error.status >= 500)
        notice =
          'The save may have completed. Reload the page to check before trying again.';
    } finally {
      busy = false;
      saving = false;
    }
  }
  function source(setting: Settings['settings'][string]) {
    if (setting.source === 'env')
      return `Environment variable ${setting.variable}. Read-only: change or remove the variable and restart the server.`;
    if (setting.source === 'file') return `Config file: ${setting.file}.`;
    return 'Default value.';
  }
</script>

<h1>Settings</h1>
{#if loading}<p>Loading settings…</p>{/if}
{#if problem}<ProblemDetails {problem} />{/if}
{#if conflict && !draft}<Button onclick={reloadConflict} disabled={busy}
    >Reload settings for review</Button
  >{/if}
{#if conflict && draft && previous && settings}
  <div class="restart" role="status">
    <p>Review your changes against the reloaded settings:</p>
    <ul>
      {#each Object.keys(draft).filter((key) => draft![key] !== previous![key]) as key (key)}<li
        >
          <strong>{key}</strong>: current {display(
            settings.settings[key]?.value,
          )}, your change {draft[key]}
        </li>{/each}
    </ul>
    <Button onclick={reapply}>Re-apply my changes</Button>
    <Button
      onclick={() => {
        draft = undefined;
        previous = undefined;
        conflict = false;
        notice = 'Your changes were discarded.';
      }}>Discard my changes</Button
    >
  </div>
{/if}
{#if settings}
  <p>Configuration file: {settings.config_file}</p>
  <p>
    New endpoints and connection tests must use loopback. Private or Compose
    hosts must be explicitly allow-listed in the configuration file or
    environment; public endpoints are never permitted.
  </p>
  {#if saving}<p role="status">
      Save in progress. It may still complete if you close or reload this page.
    </p>{/if}
  <p role="status">
    {dirty || conflict ? 'Unsaved changes' : 'No unsaved changes'}{#if notice}
      — {notice}{/if}
  </p>
  {#if settings.restart_required.length}
    <div class="restart" role="status">
      <strong>Restart required:</strong>
      {settings.restart_required.join(', ')}. The active values are shown below.
    </div>
  {:else if notice === 'Settings saved and refreshed.' && !problem}
    <p>All saved settings are active without a restart.</p>
  {/if}
  <form
    onsubmit={(event) => {
      event.preventDefault();
      void save();
    }}
    novalidate
  >
    {#each sections as section (section.title)}
      <section>
        <h2>{section.title}</h2>
        {#each section.fields as field (field.key)}
          {@const setting = settings.settings[field.key]}
          {#if setting}
            <Input
              label={field.label}
              bind:value={
                () => form[field.key], (value) => update(field.key, value)
              }
              readonly={setting.read_only}
              disabled={busy || conflict}
              hint={`${source(setting)}${setting.read_only && setting.source !== 'env' ? ' Read-only in the settings API.' : ''}${field.kind === 'hosts' ? ' Enter a JSON array of exact hosts.' : ''}${setting.active_value !== undefined ? ` Active value until restart: ${display(setting.active_value)}.` : ''}`}
              error={validation[field.key] ?? fieldErrors[field.key]}
            />
          {/if}
        {/each}
        {#if section.title === 'ComfyUI' || section.title === 'LLM'}
          <Button
            onclick={() =>
              testConnection(section.title === 'ComfyUI' ? 'comfyui' : 'llm')}
            disabled={busy ||
              conflict ||
              Object.keys(validation).some((key) =>
                key.startsWith(
                  section.title === 'ComfyUI' ? 'comfyui.' : 'llm.',
                ),
              )}>Test {section.title} connection</Button
          >
        {/if}
      </section>
    {/each}
    {#if probeProblem}<ProblemDetails problem={probeProblem} />{/if}
    {#if result && probeService}
      <div role="status">
        {#if probeService === 'comfyui'}<p>
            ComfyUI: {result.comfyui}{#if result.comfyui_reason}
              ({result.comfyui_reason === 'timeout'
                ? 'timed out'
                : 'probe failed'}){/if}
          </p>
        {:else}<p>
            LLM: {result.llm === 'not_configured'
              ? 'not configured'
              : result.llm}
          </p>{/if}
        {#if (probeService === 'comfyui' ? result.comfyui : result.llm) === 'unavailable'}<p
          >
            Check that the configured service is running and reachable, then
            test again.
          </p>{/if}
        <p>Connection tests do not save settings.</p>
      </div>
    {/if}
    <Button
      type="submit"
      variant="primary"
      disabled={busy ||
        invalid ||
        conflict ||
        !dirty ||
        Object.keys(fieldErrors).length > 0}>Save settings</Button
    >
  </form>
{/if}

<style>
  form,
  section {
    display: grid;
    gap: var(--space-4);
  }
  form,
  .restart {
    margin-block-start: var(--space-4);
  }
  section {
    border-block-start: 1px solid var(--border);
    padding-block-start: var(--space-4);
  }
  section :global(button),
  form > :global(button) {
    justify-self: start;
  }
  p + p {
    margin-block-start: var(--space-2);
  }
</style>
