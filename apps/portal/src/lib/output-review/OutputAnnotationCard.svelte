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
  {#if annotation.spec_review}
    {@const review = annotation.spec_review}
    <p class="label">
      Spec review against {review.character_id}, version {review.spec_version}.
      {review.label}
    </p>
    <ul class="checklist" aria-label="Spec checklist">
      {#each review.checklist as item (item.id)}
        <li>
          <strong>{item.id}</strong>
          ({item.requirement}, {item.category}) ·
          <span class={`verdict ${item.verdict}`}>{item.verdict}</span>
          {#if item.note}· {item.note}{/if}
        </li>
      {/each}
    </ul>
    {#if review.dropped.length}
      <p class="label">Dropped from the model answer:</p>
      <ul>
        {#each review.dropped as note, index (index)}<li>{note}</li>{/each}
      </ul>
    {/if}
    <dl>
      <div>
        <dt>Summary</dt>
        <dd>{review.summary}</dd>
      </div>
    </dl>
  {:else}
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
  {/if}
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
  .checklist {
    padding-left: var(--space-4);
  }
  .verdict {
    font-weight: var(--weight-strong);
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
