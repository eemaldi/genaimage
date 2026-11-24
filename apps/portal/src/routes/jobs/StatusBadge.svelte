<script lang="ts">
  import { describeStatus } from './run-model';
  let { status }: { status: string } = $props();
  let info = $derived(describeStatus(status));
</script>

<span class="badge"
  ><span aria-hidden="true">{info.icon}</span> {info.label}</span
>

<style>
  .badge {
    display: inline-block;
    padding: 0.125rem 0.5rem;
    background: var(--subtle);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    font-weight: 600;
  }
</style>
