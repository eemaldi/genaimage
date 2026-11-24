<script lang="ts">
  import { parseMarkdown, type InlineNode } from './markdown';

  let { text }: { text: string } = $props();
  const blocks = $derived(parseMarkdown(text));
</script>

{#snippet inline(nodes: InlineNode[])}
  {#each nodes as node, index (index)}
    {#if node.type === 'text'}{node.text}{:else if node.type === 'strong'}<strong
        >{@render inline(node.children)}</strong
      >{:else if node.type === 'emphasis'}<em
        >{@render inline(node.children)}</em
      >{:else if node.type === 'code'}<code>{node.text}</code
      >{:else if node.type === 'link'}<a
        href={node.href}
        target="_blank"
        rel="noopener noreferrer">{@render inline(node.children)}</a
      >{/if}
  {/each}
{/snippet}

<div class="markdown">
  {#each blocks as block, index (index)}
    {#if block.type === 'paragraph'}
      <p>{@render inline(block.children)}</p>
    {:else if block.type === 'heading'}
      {#if block.level === 1}<h1>{@render inline(block.children)}</h1>
      {:else if block.level === 2}<h2>{@render inline(block.children)}</h2>
      {:else if block.level === 3}<h3>{@render inline(block.children)}</h3>
      {:else if block.level === 4}<h4>{@render inline(block.children)}</h4>
      {:else if block.level === 5}<h5>{@render inline(block.children)}</h5>
      {:else}<h6>{@render inline(block.children)}</h6>{/if}
    {:else if block.type === 'code'}
      <pre><code data-language={block.language}>{block.text}</code></pre>
    {:else if block.ordered}
      <ol>
        {#each block.items as item (item)}<li>{@render inline(item)}</li>{/each}
      </ol>
    {:else}
      <ul>
        {#each block.items as item (item)}<li>{@render inline(item)}</li>{/each}
      </ul>
    {/if}
  {/each}
</div>

<style>
  .markdown :global(p + p),
  .markdown :global(:is(p, ul, ol, pre) + *) {
    margin-block-start: var(--space-2);
  }
  .markdown :global(pre) {
    overflow: auto;
    padding: var(--space-2);
    background: var(--canvas);
    border-radius: var(--radius-control);
  }
</style>
