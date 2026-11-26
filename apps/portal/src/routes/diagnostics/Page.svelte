<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import DoctorCheckList from './DoctorCheckList.svelte';
  import { describeCounts, validateReport, type DoctorReport } from './model';

  let { params: _params, query: _query }: RouteProps = $props();
  let report = $state<DoctorReport>();
  let problem = $state<Problem>();
  let running = $state(false);
  let stale = $state(false);
  let counts = $derived(report ? describeCounts(report.checks) : undefined);
  let announcement = $derived(
    running
      ? report
        ? 'Refreshing diagnostics…'
        : 'Running diagnostics…'
      : problem
        ? 'Diagnostics failed'
        : counts
          ? `Diagnostics finished: ${counts}`
          : '',
  );
  let latest = 0;
  let controller: AbortController | undefined;

  function incomplete(
    missing: string[],
    unexpected: string[],
    duplicated: string[],
  ): Problem {
    const list = (items: string[]) =>
      items.length ? items.join(', ') : 'none';
    return {
      title: 'Diagnostics report incomplete',
      detail: `The server returned an incomplete diagnostics report. Missing: ${list(missing)}; unexpected: ${list(unexpected)}; duplicated: ${list(duplicated)}.`,
      recovery:
        'Check that the server and CLI versions match, then run the diagnostics again.',
    };
  }

  function problemFrom(error: unknown): Problem {
    if (error instanceof ApiProblemError)
      return {
        ...error.problem,
        recovery:
          error.problem.recovery ||
          'Check the server logs and run the diagnostics again.',
      };
    return {
      title: 'Diagnostics could not run',
      detail: error instanceof Error ? error.message : undefined,
      recovery:
        'Start the local server with `genaimage serve`, or run `genaimage doctor` in a terminal, then run the diagnostics again.',
    };
  }

  async function run() {
    controller?.abort();
    const mine = ++latest;
    const current = (controller = new AbortController());
    running = true;
    stale = true;
    try {
      const next = await api.doctor({ signal: current.signal });
      if (mine !== latest) return;
      const check = validateReport(next);
      if (!check.valid) {
        problem = incomplete(check.missing, check.unexpected, check.duplicated);
        return;
      }
      report = next;
      problem = undefined;
      stale = false;
    } catch (error) {
      if (mine !== latest) return;
      problem = problemFrom(error);
    } finally {
      if (mine === latest) running = false;
    }
  }

  onMount(() => {
    void run();
    return () => {
      latest++;
      controller?.abort();
    };
  });
</script>

<h1>Diagnostics</h1>
<p>
  Checks the server, configuration, ComfyUI, the local LLM and the host. A run
  can take several seconds.
</p>
<p>
  <Button variant="primary" onclick={run} aria-busy={running}
    >{report ? 'Run again' : 'Run diagnostics'}</Button
  >
</p>
<div role="status">{announcement}</div>
{#if problem}<ProblemDetails {problem} />{/if}
{#if report && counts}
  <section aria-label="Diagnostics report">
    {#if stale}
      <p class="stale-note">
        <strong
          >{running ? 'Refreshing…' : 'Outdated: the last run failed'}</strong
        >
        The checks below are from the last successful run.
      </p>
    {/if}
    <h2>{stale ? 'Previous report: ' : ''}{counts}</h2>
    <DoctorCheckList checks={report.checks} />
  </section>
{/if}

<style>
  .stale-note {
    border-inline-start: 4px solid var(--danger);
    padding-inline-start: var(--space-3);
  }
</style>
