<script lang="ts">
  import {
    formatBytes,
    isRunning,
    percentComplete,
    pullStates,
    type ModelPull,
  } from './model';

  let { pull }: { pull: ModelPull } = $props();
  const percent = $derived(percentComplete(pull));
  const state = $derived(
    Object.hasOwn(pullStates, pull.state)
      ? pullStates[pull.state]
      : { label: 'Unknown state', icon: '?' },
  );
  const bytesText = $derived.by(() => {
    if (pull.completed_bytes == null) return '';
    const done = formatBytes(pull.completed_bytes);
    if (!pull.total_bytes) return `${done} downloaded, total not known yet`;
    const share = percent === undefined ? '' : ` (${percent}%)`;
    const growth = isRunning(pull)
      ? ', total can grow as more layers start'
      : '';
    return `${done} of ${formatBytes(pull.total_bytes)}${share}${growth}`;
  });
</script>

<div class="pull-progress">
  <p>
    <span class="badge"
      ><span aria-hidden="true">{state.icon}</span> {state.label}</span
    >
    {pull.status}{pull.layer ? ` (layer ${pull.layer})` : ''}
  </p>
  {#if bytesText}<p>{bytesText}</p>{/if}
  {#if isRunning(pull) && percent !== undefined}
    <progress max="100" value={percent} aria-label={`Progress of ${pull.name}`}
    ></progress>
  {/if}
  {#if pull.error}<p class="error">{pull.error}</p>{/if}
</div>

<style>
  .pull-progress p {
    margin-block: var(--space-1);
  }
  .badge {
    font-weight: var(--weight-strong);
    margin-inline-end: var(--space-2);
  }
  progress {
    inline-size: 100%;
  }
  .error {
    color: var(--danger);
  }
</style>
