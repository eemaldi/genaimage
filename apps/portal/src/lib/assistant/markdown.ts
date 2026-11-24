export type InlineNode =
  | { type: 'text'; text: string }
  | { type: 'strong' | 'emphasis'; children: InlineNode[] }
  | { type: 'code'; text: string }
  | { type: 'link'; href: string; children: InlineNode[] };

export type MarkdownBlock =
  | { type: 'paragraph'; children: InlineNode[] }
  | { type: 'heading'; level: number; children: InlineNode[] }
  | { type: 'code'; language?: string; text: string }
  | { type: 'list'; ordered: boolean; items: InlineNode[][] };

function linkTarget(value: string): string | undefined {
  try {
    const url = new URL(value);
    return url.protocol === 'http:' || url.protocol === 'https:'
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}

export function parseInline(text: string): InlineNode[] {
  const nodes: InlineNode[] = [];
  let plain = '';
  const flush = () => {
    if (plain) nodes.push({ type: 'text', text: plain });
    plain = '';
  };
  for (let index = 0; index < text.length;) {
    if (text[index] === '\\' && index + 1 < text.length) {
      plain += text[index + 1];
      index += 2;
      continue;
    }
    if (text[index] === '`') {
      const end = text.indexOf('`', index + 1);
      if (end > index) {
        flush();
        nodes.push({ type: 'code', text: text.slice(index + 1, end) });
        index = end + 1;
        continue;
      }
    }
    if (text[index] === '[') {
      const labelEnd = text.indexOf(']', index + 1);
      if (labelEnd !== -1 && text[labelEnd + 1] === '(') {
        let targetEnd = labelEnd + 2;
        let depth = 0;
        for (; targetEnd < text.length; targetEnd++) {
          if (text[targetEnd] === '(') depth++;
          else if (text[targetEnd] === ')') {
            if (depth === 0) break;
            depth--;
          }
        }
        if (targetEnd < text.length) {
          const href = linkTarget(text.slice(labelEnd + 2, targetEnd).trim());
          flush();
          const children = parseInline(text.slice(index + 1, labelEnd));
          if (href) nodes.push({ type: 'link', href, children });
          else nodes.push(...children);
          index = targetEnd + 1;
          continue;
        }
      }
    }
    const delimiter = text.startsWith('**', index)
      ? '**'
      : text.startsWith('__', index)
        ? '__'
        : text[index] === '*'
          ? '*'
          : text[index] === '_'
            ? '_'
            : undefined;
    if (delimiter) {
      const end = text.indexOf(delimiter, index + delimiter.length);
      if (end > index + delimiter.length) {
        flush();
        nodes.push({
          type: delimiter.length === 2 ? 'strong' : 'emphasis',
          children: parseInline(text.slice(index + delimiter.length, end)),
        });
        index = end + delimiter.length;
        continue;
      }
    }
    plain += text[index++];
  }
  flush();
  return nodes;
}

export function parseMarkdown(source: string): MarkdownBlock[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n');
  const blocks: MarkdownBlock[] = [];
  for (let index = 0; index < lines.length;) {
    if (!lines[index].trim()) {
      index++;
      continue;
    }
    const fence = lines[index].match(/^```\s*([^\s`]*)\s*$/);
    if (fence) {
      const code: string[] = [];
      index++;
      while (index < lines.length && !/^```\s*$/.test(lines[index]))
        code.push(lines[index++]);
      if (index < lines.length) index++;
      blocks.push({
        type: 'code',
        language: fence[1] || undefined,
        text: code.join('\n'),
      });
      continue;
    }
    const heading = lines[index].match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      blocks.push({
        type: 'heading',
        level: heading[1].length,
        children: parseInline(heading[2]),
      });
      index++;
      continue;
    }
    const listItem = lines[index].match(/^\s*([-*+]|\d+[.)])\s+(.+)$/);
    if (listItem) {
      const ordered = /^\d/.test(listItem[1]);
      const items: InlineNode[][] = [];
      while (index < lines.length) {
        const item = lines[index].match(/^\s*([-*+]|\d+[.)])\s+(.+)$/);
        if (!item || /^\d/.test(item[1]) !== ordered) break;
        items.push(parseInline(item[2]));
        index++;
      }
      blocks.push({ type: 'list', ordered, items });
      continue;
    }
    const paragraph: string[] = [];
    while (
      index < lines.length &&
      lines[index].trim() &&
      !/^```/.test(lines[index]) &&
      !/^#{1,6}\s+/.test(lines[index]) &&
      !/^\s*([-*+]|\d+[.)])\s+/.test(lines[index])
    )
      paragraph.push(lines[index++].trim());
    blocks.push({
      type: 'paragraph',
      children: parseInline(paragraph.join('\n')),
    });
  }
  return blocks;
}
