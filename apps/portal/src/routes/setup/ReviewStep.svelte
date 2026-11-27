<script lang="ts">
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { Settings } from '../settings/form';
  import {
    isAbort,
    problemFrom,
    type Choices,
    type ReviewEntry,
  } from './model';

  let {
    entries,
    patch,
    owned,
    checkpoint,
    settings,
    choices,
    etag,
    saved,
    unverified,
    onSaved,
    reload,
  }: {
    entries: ReviewEntry[];
    patch: Record<string, string>;
    owned: { key: string; label: string; value: string; variable?: string }[];
    checkpoint?: string;
    settings: Settings;
    choices: Choices;
    etag: string;
    saved: boolean;
    unverified: string[];
    onSaved: (snapshot: {
      settings: Settings;
      etag: string;
      patch: Record<string, string>;
    }) => void;
    reload: () => Promise<void>;
  } = $props();

  let saving = $state(false);
  let problem = $state<Problem>();
  let conflict = $state(false);
  let unknown = $state(false);
  let outcome = $state('');

  async function save() {
    if (saving) return;
    saving = true;
    problem = undefined;
    conflict = false;
    unknown = false;
    outcome = '';
    try {
      const snapshot = await api.putSettingsWithRevision(patch, etag);
      outcome = 'Configuration saved.';
      onSaved({ settings: snapshot.settings, etag: snapshot.etag, patch });
    } catch (error) {
      if (isAbort(error)) return;
      conflict = error instanceof ApiProblemError && error.status === 412;
      const rejected =
        error instanceof ApiProblemError &&
        error.status >= 400 &&
        error.status < 500;
      unknown = !rejected;
      problem = problemFrom(
        error,
        rejected
          ? 'The configuration was not saved'
          : 'The outcome of the save is unknown',
        conflict
          ? 'The settings changed since the wizard loaded them. Reload the current settings and review the values again.'
          : rejected
            ? 'Nothing was saved. Fix the problem above and save again.'
            : 'The server may or may not have saved the configuration. Reload the current settings to see what is saved, then review the values again.',
      );
    } finally {
      saving = false;
    }
  }

  async function refresh() {
    await reload();
    problem = undefined;
    conflict = false;
    unknown = false;
  }
</script>

<p>
  Review everything the wizard will write to
  <code>{settings.config_file}</code>{#if !settings.config_file_exists}
    (the file does not exist yet and will be created){/if}.
</p>
{#if entries.length}
  <table class="setup-table" aria-label="Values to save">
    <thead>
      <tr
        ><th scope="col">Setting</th><th scope="col">Current</th><th scope="col"
          >New value</th
        ></tr
      >
    </thead>
    <tbody>
      {#each entries as entry (entry.key)}
        <tr data-key={entry.key}>
          <th scope="row">{entry.label} <code>{entry.key}</code></th>
          <td>{entry.from || 'not set'}</td>
          <td>{entry.to}{entry.from === entry.to ? ' (unchanged)' : ''}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{:else}
  <p>
    Nothing will be saved: every setting is either unchosen or set by the
    environment.
  </p>
{/if}
{#if owned.length}
  <h3>Set by the environment (not changed)</h3>
  <ul>
    {#each owned as item (item.key)}
      <li>
        {item.label} <code>{item.key}</code>: {item.value ||
          'not set'}{#if item.variable}
          from <code>{item.variable}</code>{/if}
      </li>
    {/each}
  </ul>
{/if}
{#if checkpoint}
  <p>
    Checkpoint for the test generation: <strong>{checkpoint}</strong> (not saved to
    the configuration).
  </p>
{/if}
{#if choices.llmSkipped}<p>
    LLM setup was skipped, so no LLM values are saved.
  </p>{/if}
<p>
  Saved values take effect only after you restart <code>genaimage serve</code>;
  they are not live yet.
</p>
{#if unverified.length}
  <div class="setup-problem" role="note">
    <p><strong>Not verified for these choices:</strong></p>
    <ul>
      {#each unverified as item (item)}<li>{item}</li>{/each}
    </ul>
    <p>You can still save, but nothing above has confirmed these work.</p>
  </div>
{/if}
{#if problem}<ProblemDetails {problem} />{/if}
{#if conflict || unknown}<p>
    <Button onclick={refresh}>Reload current settings</Button>
  </p>{/if}
<p>
  <Button
    variant="primary"
    onclick={save}
    disabled={saving || conflict || unknown || !entries.length}
    aria-busy={saving}>{saved ? 'Save again' : 'Save configuration'}</Button
  >
</p>
<div role="status">{saving ? 'Saving…' : outcome}</div>
