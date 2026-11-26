<script lang="ts">
  import { api } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import DoctorCheckList from '../diagnostics/DoctorCheckList.svelte';
  import {
    describeCounts,
    validateReport,
    type DoctorCheck,
  } from '../diagnostics/model';
  import { isAbort, latest, problemFrom, type SetupStatus } from './model';

  let {
    status,
    refresh,
  }: { status: SetupStatus; refresh: () => Promise<void> } = $props();

  let checks = $state<DoctorCheck[]>();
  let problem = $state<Problem>();
  let running = $state(false);
  let checking = $state(false);
  const request = latest();
  const relevant = /^(config|comfyui\.|llm\.)/;

  async function recheck() {
    checking = true;
    try {
      await refresh();
    } finally {
      checking = false;
    }
  }

  async function diagnose() {
    const { signal, live } = request.begin();
    running = true;
    problem = undefined;
    try {
      const report = await api.doctor({ signal });
      if (!live()) return;
      if (!validateReport(report).valid)
        throw new Error('The diagnostics report was incomplete.');
      checks = report.checks.filter((check) => relevant.test(check.id));
    } catch (error) {
      if (isAbort(error) || !live()) return;
      checks = undefined;
      problem = problemFrom(
        error,
        'Diagnostics could not run',
        'Check that the GenAImage server is running, then run the diagnostics again.',
      );
    } finally {
      if (live()) running = false;
    }
  }
</script>

{#if status.restart_required.length}
  <div class="setup-problem" role="alert">
    <p>
      <strong>Restart required.</strong> The saved configuration is not in use yet.
      These settings change only after a restart:
    </p>
    <ul>
      {#each status.restart_required as key (key)}<li>
          <code>{key}</code>
        </li>{/each}
    </ul>
    <p class="setup-fix">
      <strong>What to do:</strong> stop <code>genaimage serve</code> (Ctrl+C) and
      start it again. With Docker Compose, restart the GenAImage container. The portal
      cannot restart the server for you.
    </p>
  </div>
  <p>
    <Button onclick={recheck} aria-busy={checking}>Check again</Button>
  </p>
{:else}
  <p role="status">
    The server reports no pending restart: the running server uses the saved
    configuration.
  </p>
  <p>
    <Button onclick={diagnose} aria-busy={running}>Run diagnostics</Button>
  </p>
  {#if problem}<ProblemDetails {problem} />{/if}
  {#if checks}
    <h3>{describeCounts(checks)}</h3>
    <DoctorCheckList {checks} />
  {/if}
{/if}
