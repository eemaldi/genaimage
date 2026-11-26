<script lang="ts">
  import {
    checkLabel,
    describeCheckStatus,
    splitInlineCode,
    type DoctorCheck,
  } from './model';

  let { checks }: { checks: readonly DoctorCheck[] } = $props();
</script>

{#snippet inline(text: string)}
  {#each splitInlineCode(text) as segment, index (index)}
    {#if segment.code}<code>{segment.text}</code>{:else}{segment.text}{/if}
  {/each}
{/snippet}

<ul class="doctor-checks">
  {#each checks as check, position (position)}
    {@const info = describeCheckStatus(check.status)}
    <li class="doctor-check {info.tone}" data-check={check.id}>
      <h3>
        {checkLabel(check.id)}
        <span class="badge"
          ><span aria-hidden="true">{info.icon}</span> {info.label}</span
        >
      </h3>
      <p>{@render inline(check.summary)}</p>
      {#if check.items.length}
        <ul>
          {#each check.items as item, index (index)}
            <li>{@render inline(item)}</li>
          {/each}
        </ul>
      {/if}
      {#if check.status !== 'ok'}
        <p class="fix">
          <strong>How to fix:</strong>
          {@render inline(check.fix ?? 'No fix was provided by the server.')}
        </p>
      {/if}
    </li>
  {/each}
</ul>

<style>
  .doctor-checks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-3);
  }
  .doctor-check {
    background: var(--surface);
    border: 1px solid var(--border);
    border-inline-start-width: 4px;
    border-radius: var(--radius-control);
    padding: var(--space-3) var(--space-4);
  }
  .doctor-check.error {
    border-color: var(--danger);
    background: var(--danger-surface);
  }
  .doctor-check.warning {
    border-style: dashed;
  }
  .doctor-check h3 {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
    margin: 0;
  }
  .doctor-check p,
  .doctor-check ul {
    margin: var(--space-2) 0 0;
  }
  .fix {
    font-weight: 600;
  }
  .badge {
    padding: 0.125rem 0.5rem;
    background: var(--subtle);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    font-size: var(--text-small);
  }
</style>
