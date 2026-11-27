import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import type { api } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import { mockNativeDialog } from '../../test/dialog';
import BatchOutputs from './BatchOutputs.svelte';
import { batch, item } from './fixtures';

mockNativeDialog();

const fake = vi.hoisted(() => ({
  listOutputs: vi.fn<typeof api.listOutputs>(),
  getRecordedRun: vi.fn<typeof api.getRecordedRun>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

type Output = components['schemas']['OutputResponse'];
const output = (run: string): Output => ({
  id: `out-${run}`,
  run_id: run,
  byte_size: 1,
  content_type: 'image/png',
  created_at: '2026-11-27T10:00:00Z',
  kind: 'image',
  node_id: '9',
  sha256: 'a'.repeat(64),
  source_filename: `${run}.png`,
  tags: [],
  workflow_id: 'portrait',
});

beforeEach(() => {
  fake.listOutputs.mockImplementation(async (query) => ({
    outputs: [output(query!.run!)],
    next_cursor: null,
  }));
  fake.getRecordedRun.mockResolvedValue({ id: 'x', parameters: {} } as never);
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const items = [
  item(2, {
    submission: 'submitted',
    run_id: 'run-2',
    run_status: 'completed',
    sweep_values: { seed: 42 },
  }),
  item(0, {
    submission: 'submitted',
    run_id: 'run-0',
    run_status: 'completed',
    sweep_values: { seed: 18446744073709551615n },
  }),
  item(1, {
    submission: 'submitted',
    run_id: 'run-1',
    run_status: 'running',
    sweep_values: { seed: 7 },
  }),
];

test('only runs the backend completed are queried, and cards follow item order with sweep labels', async () => {
  render(BatchOutputs, { batch: batch({ state: 'submitted', items }) });
  expect(screen.getByText(/2 of 3 so far/)).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Load outputs' }));
  const grid = await screen.findByRole('list', { name: 'Batch outputs' });
  expect(
    fake.listOutputs.mock.calls.map(([query]) => query!.run).sort(),
  ).toEqual(['run-0', 'run-2']);
  const labels = within(grid)
    .getAllByText(/^Run \d/)
    .map((node) => node.textContent);
  expect(labels).toEqual([
    'Run 1 · seed=18446744073709551615',
    'Run 3 · seed=42',
  ]);
});

test('selected outputs open a comparison headed by their sweep values', async () => {
  render(BatchOutputs, { batch: batch({ state: 'submitted', items }) });
  await userEvent.click(screen.getByRole('button', { name: 'Load outputs' }));
  await screen.findByRole('list', { name: 'Batch outputs' });
  expect(
    screen
      .getByRole('button', { name: /Compare selected \(0\)/ })
      .hasAttribute('disabled'),
  ).toBe(true);
  for (const box of screen.getAllByRole('checkbox')) await userEvent.click(box);
  await userEvent.click(
    screen.getByRole('button', { name: /Compare selected \(2\)/ }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole('heading', {
        name: '1. Run 1 · seed=18446744073709551615',
      }),
    ).toBeTruthy(),
  );
  expect(
    screen.getByRole('heading', { name: '2. Run 3 · seed=42' }),
  ).toBeTruthy();
});

test('nothing is requested when no run is confirmed complete', () => {
  render(BatchOutputs, {
    batch: batch({
      items: [
        item(0, { submission: 'submitted', run_id: 'r', run_status: 'queued' }),
      ],
    }),
  });
  expect(
    screen
      .getByRole('button', { name: 'Load outputs' })
      .hasAttribute('disabled'),
  ).toBe(true);
  expect(fake.listOutputs).not.toHaveBeenCalled();
});
