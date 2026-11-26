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
  DOCTOR_CHECK_IDS,
  checkLabel,
  describeCounts,
  splitInlineCode,
  summarise,
  validateReport,
  type DoctorCheck,
} from './model';

const fake = vi.hoisted(() => ({ doctor: vi.fn<typeof api.doctor>() }));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

const cycle = ['ok', 'warning', 'error', 'skipped'] as const;
function report(summary = 'Summary', offset = 0) {
  return {
    checks: DOCTOR_CHECK_IDS.map((id, index): DoctorCheck => {
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
    name: '3 of 12 checks OK · 3 errors · 3 warnings · 3 skipped',
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

const signalOf = (call: number) => fake.doctor.mock.calls[call][0]!.signal!;

test('Run again keeps the previous report visible, marked refreshing', async () => {
  const user = userEvent.setup();
  fake.doctor.mockResolvedValueOnce(report('Old'));
  render(Page, props);
  await screen.findByText('Old server');
  fake.doctor.mockReturnValueOnce(new Promise(() => {}));
  await user.click(screen.getByRole('button', { name: 'Run again' }));
  expect(screen.getByText('Old server')).toBeTruthy();
  expect(screen.getByText('Refreshing…')).toBeTruthy();
  expect(screen.getByRole('status').textContent).toContain('Refreshing');
  expect(signalOf(0).aborted).toBe(true);
  expect(signalOf(1).aborted).toBe(false);
});

test('a failed re-run marks the old report outdated, not current', async () => {
  const user = userEvent.setup();
  fake.doctor.mockResolvedValueOnce(report('Old'));
  render(Page, props);
  await screen.findByText('Old server');
  fake.doctor.mockRejectedValueOnce(
    new ApiNetworkError('Check the server.', new Error('x')),
  );
  await user.click(screen.getByRole('button', { name: 'Run again' }));
  await screen.findByRole('alert');
  expect(screen.getByText('Outdated: the last run failed')).toBeTruthy();
  expect(screen.getByRole('status').textContent).toBe('Diagnostics failed');
  const heading = screen.getByRole('heading', { level: 2 });
  expect(heading.textContent).toContain('Previous report');
  fake.doctor.mockResolvedValueOnce(report('New'));
  await user.click(screen.getByRole('button', { name: 'Run again' }));
  await screen.findByText('New server');
  expect(screen.queryByText(/Outdated/)).toBeNull();
  expect(screen.getByRole('status').textContent).toContain(
    'Diagnostics finished: 3 of 12 checks OK',
  );
});

test('unmounting aborts the in-flight request without showing an error', async () => {
  fake.doctor.mockReturnValueOnce(new Promise(() => {}));
  render(Page, props);
  expect(signalOf(0).aborted).toBe(false);
  cleanup();
  expect(signalOf(0).aborted).toBe(true);
  expect(screen.queryByRole('alert')).toBeNull();
});

test('a non-ok check without a fix says so', () => {
  const checks = [
    { id: 'gpu', status: 'error', summary: 's', items: [], fix: null },
  ] as DoctorCheck[];
  render(DoctorCheckList, { checks });
  expect(screen.getByText(/No fix was provided by the server\./)).toBeTruthy();
});

test('summary uses singular and plural forms', () => {
  const one = [
    { id: 'gpu', status: 'error', summary: '', items: [], fix: 'x' },
    { id: 'disk', status: 'warning', summary: '', items: [], fix: 'x' },
  ] as DoctorCheck[];
  expect(describeCounts(one)).toBe(
    '0 of 2 checks OK · 1 error · 1 warning · 0 skipped',
  );
});

const broken = {
  empty: { checks: [] },
  missing: { checks: report().checks.slice(1) },
  duplicate: { checks: [...report().checks, report().checks[0]] },
  unknown: { checks: [...report().checks, { ...report().checks[0], id: 'x' }] },
  'non-array': { checks: 'nope' },
};
test.each(Object.entries(broken))(
  'an invalid report (%s) is rejected, never counted',
  async (_name, data) => {
    expect(validateReport(data).valid).toBe(false);
    fake.doctor.mockResolvedValue(data as never);
    render(Page, props);
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('incomplete diagnostics report');
    expect(document.querySelector('[data-check]')).toBeNull();
    expect(screen.queryByText(/checks OK/)).toBeNull();
  },
);

test('validation names the missing, unexpected and duplicated ids', () => {
  const data = broken.duplicate;
  expect(validateReport(data).duplicated).toEqual(['server']);
  expect(validateReport(broken.missing).missing).toEqual(['server']);
  expect(validateReport(broken.unknown).unexpected).toEqual(['x']);
  expect(validateReport(report()).valid).toBe(true);
  expect(validateReport(null).valid).toBe(false);
});

test('duplicate ids do not crash the list', () => {
  render(DoctorCheckList, { checks: broken.duplicate.checks });
  expect(document.querySelectorAll('[data-check="server"]')).toHaveLength(2);
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
  await waitFor(() =>
    expect(screen.getByRole('status').textContent).toContain('finished'),
  );
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
