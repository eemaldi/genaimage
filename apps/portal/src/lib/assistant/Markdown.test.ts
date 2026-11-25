import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import Markdown from './Markdown.svelte';

it('renders raw HTML as literal text instead of active elements', () => {
  const { container } = render(Markdown, {
    text: '<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>',
  });
  expect(container.querySelector('script')).toBeNull();
  expect(container.querySelector('img')).toBeNull();
  expect(screen.getByText('<script>alert(1)</script>')).toBeTruthy();
});

it('rejects unsafe links', () => {
  const { container } = render(Markdown, {
    text: '[javascript](javascript:alert(1)) [data](data:text/html,x)',
  });
  expect(container.querySelector('a')).toBeNull();
  expect(container.textContent).toContain('javascript data');
});

it('allows only safe links and adds safe browsing attributes', () => {
  render(Markdown, { text: '[site](https://example.test/path)' });
  const link = screen.getByRole('link', { name: 'site' });
  expect(link.getAttribute('target')).toBe('_blank');
  expect(link.getAttribute('rel')).toBe('noopener noreferrer');
});

it('renders nested emphasis and unclosed fenced code', () => {
  const { container } = render(Markdown, {
    text: '**strong *nested* text**\n\n```rust\nfn main() {',
  });
  const strong = container.querySelector('strong');
  expect(strong?.querySelector('em')?.textContent).toBe('nested');
  expect(container.querySelector('pre code')?.textContent).toBe('fn main() {');
  expect(container.querySelector('[data-language="rust"]')).toBeTruthy();
});

describe('markdown blocks', () => {
  it('renders headings, lists and inline code', () => {
    render(Markdown, { text: '# Title\n\n- one\n- `two`' });
    expect(
      screen.getByRole('heading', { level: 1, name: 'Title' }),
    ).toBeTruthy();
    expect(screen.getByRole('list')).toBeTruthy();
    expect(screen.getByText('two', { selector: 'code' })).toBeTruthy();
  });
});
