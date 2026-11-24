<script lang="ts">
  import type { OutputAnnotation } from './model';

  let { annotation }: { annotation: OutputAnnotation } = $props();
  const date = new Intl.DateTimeFormat('en-GB', {
    dateStyle: 'medium',
    timeStyle: 'medium',
  });
  const time = (timestamp: string) =>
    Number.isFinite(Date.parse(timestamp))
      ? date.format(new Date(timestamp))
      : timestamp;
</script>

<article class="annotation" aria-label={`Model opinion by ${annotation.model}`}>
  <p class="label">
    <strong>Model opinion</strong> · {annotation.model} ·
    <time datetime={annotation.created_at}>{time(annotation.created_at)}</time>
  </p>
  <dl>
    <div>
      <dt>Caption</dt>
      <dd>{annotation.caption}</dd>
    </div>
    <div>
      <dt>Adherence critique</dt>
      <dd>{annotation.adherence_critique}</dd>
    </div>
    <div>
      <dt>Tags</dt>
      <dd>{annotation.tags.length ? annotation.tags.join(', ') : 'None'}</dd>
    </div>
  </dl>
  <details>
    <summary>Details</summary>
    <dl>
      <div>
        <dt>Prompt</dt>
        <dd><pre>{annotation.prompt}</pre></dd>
      </div>
      <div>
        <dt>Raw response</dt>
        <dd><pre>{annotation.raw_response}</pre></dd>
      </div>
    </dl>
  </details>
</article>

<style>
  .annotation {
    padding: var(--space-3);
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .label {
    margin-block: 0 var(--space-2);
    color: var(--muted);
    font-size: var(--text-small);
  }
  dl {
    display: grid;
    gap: var(--space-2);
    margin: 0;
  }
  dt {
    font-weight: var(--weight-strong);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  details {
    margin-top: var(--space-3);
  }
  summary {
    cursor: pointer;
  }
  pre {
    margin: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
</style>
