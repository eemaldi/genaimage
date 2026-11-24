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
  let fieldErrors = $state<Record<string, string>>({});
  let busy = $state(false);
  let loading = $state(true);
  let notice = $state('');
  let result = $state<components['schemas']['SettingsTestResponse']>();
  let currentHash = window.location.hash;
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
  }
  function load(response: Settings) {
    settings = response;
    form = formFrom(response);
    fieldErrors = {};
  }
  onMount(() => {
    let mounted = true;
    api
      .getSettings()
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
      if (dirty || busy) {
        event.preventDefault();
        event.returnValue = '';
      }
    }
    function guard(event: HashChangeEvent) {
      if (
        (dirty || busy) &&
        !window.confirm(
          'You have unsaved changes. Leave settings and discard them?',
        )
      ) {
        event.stopImmediatePropagation();
        window.history.replaceState(
          window.history.state,
          '',
          currentHash || '#/settings',
        );
      } else currentHash = window.location.hash;
    }
    window.addEventListener('beforeunload', beforeUnload);
    window.addEventListener('hashchange', guard, true);
    return () => {
      mounted = false;
      window.removeEventListener('beforeunload', beforeUnload);
      window.removeEventListener('hashchange', guard, true);
    };
  });
  async function testConnection() {
    if (!settings || invalid || busy) return;
    busy = true;
    problem = undefined;
    fieldErrors = {};
    result = undefined;
    try {
      result = await api.testSettings(patchFrom(form, settings));
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }
  async function save() {
    if (
      !settings ||
      invalid ||
      !dirty ||
      busy ||
      Object.keys(fieldErrors).length
    )
      return;
    busy = true;
    problem = undefined;
    fieldErrors = {};
    notice = '';
    try {
      const saved = await api.putSettings(patchFrom(form, settings));
      load(saved);
      try {
        load(await api.getSettings());
        notice = 'Settings saved and refreshed.';
      } catch (error) {
        notice =
          'Settings were saved, but could not be refreshed. Reload the page to verify them.';
        showError(error);
      }
    } catch (error) {
      showError(error);
      if (!(error instanceof ApiProblemError) || error.status >= 500)
        notice =
          'The save may have completed. Reload the page to check before trying again.';
    } finally {
      busy = false;
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
{#if settings}
  <p>Configuration file: {settings.config_file}</p>
  <p>
    New endpoints and connection tests must use loopback. Private or Compose
    hosts must be explicitly allow-listed in the configuration file or
    environment; public endpoints are never permitted.
  </p>
  <p role="status">
    {dirty ? 'Unsaved changes' : 'No unsaved changes'}{#if notice}
      — {notice}{/if}
  </p>
  {#if settings.restart_required.length}
    <div class="restart" role="status">
      <strong>Restart required:</strong>
      {settings.restart_required.join(', ')}. The active values are shown below.
    </div>
  {:else if notice && !problem}
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
              disabled={busy}
              hint={`${source(setting)}${setting.read_only && setting.source !== 'env' ? ' Read-only in the settings API.' : ''}${field.kind === 'hosts' ? ' Enter a JSON array of exact hosts.' : ''}${setting.active_value !== undefined ? ` Active value until restart: ${display(setting.active_value)}.` : ''}`}
              error={validation[field.key] ?? fieldErrors[field.key]}
            />
          {/if}
        {/each}
        {#if section.title === 'ComfyUI' || section.title === 'LLM'}
          <Button onclick={testConnection} disabled={busy || invalid}
            >Test {section.title} connection</Button
          >
        {/if}
      </section>
    {/each}
    {#if result}
      <div role="status">
        <p>
          ComfyUI: {result.comfyui}{#if result.comfyui_reason}
            ({result.comfyui_reason === 'timeout'
              ? 'timed out'
              : 'probe failed'}){/if}
        </p>
        <p>
          LLM: {result.llm === 'not_configured' ? 'not configured' : result.llm}
        </p>
        {#if result.comfyui === 'unavailable' || result.llm === 'unavailable'}<p
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
