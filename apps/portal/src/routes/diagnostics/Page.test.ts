import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import {
  ApiNetworkError,
  ApiProblemError,
  type api,
} from '../../lib/api/client';
import Page from './Page.svelte';
import DoctorCheckList from './DoctorCheckList.svelte';
import {
  checkLabel,
  splitInlineCode,
  summarise,
  type DoctorCheck,
} from './model';

const fake = vi.hoisted(() => ({ doctor: vi.fn<typeof api.doctor>() }));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

const ids = [
  'server',
  'config',
  'comfyui.reachable',
  'comfyui.version',
  'comfyui.object_info',
  'workflows.nodes',
  'llm.reachable',
  'llm.text',
  'llm.vision',
  'gpu',
  'disk',
  'ports',
];
const cycle = ['ok', 'warning', 'error', 'skipped'] as const;
function report(summary = 'Summary', offset = 0) {
  return {
    checks: ids.map((id, index): DoctorCheck => {
      const status = cycle[(index + offset) % 4];
      return {
        id,
        status,
        summary: `${summary} ${id}`,
        items: index === 1 ? ['first item'] : [],
        fix: status === 'ok' ? null : `Fix ${id} via \`llm.base_url\``,
      };
    }),
  };
}
const props = { params: {}, query: new URLSearchParams() };
afterEach(() => {
  cleanup();
  fake.doctor.mockReset();
});

test('renders all 12 checks, with fixes only for non-ok checks', async () => {
  const data = report();
  fake.doctor.mockResolvedValue(data);
  render(Page, props);
  await screen.findByRole('heading', {
    name: /3 errors, 3 warnings, 3 skipped/,
  });
  expect(document.querySelectorAll('[data-check]')).toHaveLength(12);
  for (const check of data.checks) {
    const item = document.querySelector<HTMLElement>(
      `[data-check="${check.id}"]`,
    )!;
    expect(within(item).getByRole('heading').textContent).toContain(
      checkLabel(check.id),
    );
    expect(within(item).getByText(check.summary)).toBeTruthy();
    expect(item.textContent).toContain(
      { ok: 'OK', warning: 'Warning', error: 'Error', skipped: 'Skipped' }[
        check.status
      ],
    );
    expect(item.textContent?.includes('How to fix')).toBe(
      check.status !== 'ok',
    );
  }
  expect(screen.getAllByText('llm.base_url')[0].tagName).toBe('CODE');
  expect(screen.getByText('first item')).toBeTruthy();
});

test('an unexpected status is never shown as ok', () => {
  const checks = [
    { id: 'gpu', status: 'fine', summary: 's', items: [], fix: null },
  ] as unknown as DoctorCheck[];
  render(DoctorCheckList, { checks });
  const item = document.querySelector<HTMLElement>('[data-check="gpu"]')!;
  expect(item.textContent).toContain('Unknown status');
  expect(item.textContent).not.toMatch(/\bOK\b/);
  expect(item.className).toContain('error');
  expect(summarise(checks).errors).toBe(1);
});

test('a network failure shows an actionable error, not an empty report', async () => {
  fake.doctor.mockRejectedValue(
    new ApiNetworkError('Check the server.', new Error('x')),
  );
  render(Page, props);
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('genaimage serve');
  expect(document.querySelector('[data-check]')).toBeNull();
});

test('a problem response shows its title and recovery', async () => {
  fake.doctor.mockRejectedValue(
    new ApiProblemError({
      status: 500,
      type: 'about:blank',
      title: 'Doctor broke',
      detail: 'boom',
      recovery: 'Do the thing.',
    }),
  );
  render(Page, props);
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('Doctor broke');
  expect(alert.textContent).toContain('Do the thing.');
});

test('Run again keeps the previous report visible, marked stale', async () => {
  const user = userEvent.setup();
  fake.doctor.mockResolvedValueOnce(report('Old'));
  render(Page, props);
  await screen.findByText('Old server');
  fake.doctor.mockReturnValueOnce(new Promise(() => {}));
  await user.click(screen.getByRole('button', { name: 'Run again' }));
  expect(screen.getByText('Old server')).toBeTruthy();
  expect(screen.getByRole('status').textContent).toContain('stale');
});

test('an older slow response never overwrites a newer one', async () => {
  const user = userEvent.setup();
  let resolveOld!: (value: ReturnType<typeof report>) => void;
  fake.doctor.mockReturnValueOnce(
    new Promise((resolve) => {
      resolveOld = resolve;
    }),
  );
  render(Page, props);
  fake.doctor.mockResolvedValueOnce(report('New'));
  await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));
  await screen.findByText('New server');
  resolveOld(report('Old'));
  await Promise.resolve();
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe(''));
  expect(screen.queryByText('Old server')).toBeNull();
  expect(screen.getByText('New server')).toBeTruthy();
});

test('inline code splitting keeps unmatched backticks as text', () => {
  expect(splitInlineCode('set `a.b` now')).toEqual([
    { text: 'set ', code: false },
    { text: 'a.b', code: true },
    { text: ' now', code: false },
  ]);
  expect(splitInlineCode('odd ` tick')).toEqual([
    { text: 'odd ` tick', code: false },
  ]);
});
