<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import DoctorCheckList from './DoctorCheckList.svelte';
  import { summarise, type DoctorReport } from './model';

  let { params: _params, query: _query }: RouteProps = $props();
  let report = $state<DoctorReport>();
  let problem = $state<Problem>();
  let running = $state(false);
  let counts = $derived(report ? summarise(report.checks) : undefined);
  let latest = 0;
  let controller: AbortController | undefined;

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
    try {
      const next = await api.doctor({ signal: current.signal });
      if (mine !== latest) return;
      report = next;
      problem = undefined;
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
<div role="status">
  {#if running}{report
      ? 'Running again. The previous report is stale until this finishes.'
      : 'Running diagnostics…'}{/if}
</div>
{#if problem}<ProblemDetails {problem} />{/if}
{#if report && counts}
  <section class:stale={running} aria-label="Diagnostics report">
    <h2>
      {counts.errors} errors, {counts.warnings} warnings, {counts.skipped} skipped
      {#if running}<span class="stale-note"> (stale)</span>{/if}
    </h2>
    <DoctorCheckList checks={report.checks} />
  </section>
{/if}

<style>
  .stale {
    opacity: 0.65;
  }
</style>
