<script module lang="ts">
  export type Problem = {
    type?: string;
    title?: string;
    status?: number;
    detail?: string;
    instance?: string;
    recovery?: string;
    errors?: readonly { key: string; reason: string }[];
  };
</script>

<script lang="ts">
  let { problem }: { problem: Problem } = $props();
</script>

<div class="ui-problem" role="alert">
  <p class="ui-problem-title">{problem.title || 'Something went wrong'}</p>
  {#if problem.detail}<p>{problem.detail}</p>{/if}
  {#if problem.recovery}<p>
      <strong>What to do next:</strong>
      {problem.recovery}
    </p>{/if}
  {#if problem.errors?.length}
    <ul>
      {#each problem.errors as { key, reason }, index (index)}
        <li><strong>{key}:</strong> {reason}</li>
      {/each}
    </ul>
  {/if}
</div>
