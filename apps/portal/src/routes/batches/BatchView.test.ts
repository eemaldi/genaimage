import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { writable } from 'svelte/store';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, type api } from '../../lib/api/client';
import type {
  ConnectionStatus,
  EventEnvelope,
  ResyncSignal,
} from '../../lib/api/events';
import { mockNativeDialog } from '../../test/dialog';
import BatchView from './BatchView.svelte';
import { batch, item } from './fixtures';
import { stateLabels } from './labels';

mockNativeDialog();

const fake = vi.hoisted(() => ({
  client: {
    getBatch: vi.fn<typeof api.getBatch>(),
    resumeBatch: vi.fn<typeof api.resumeBatch>(),
    listOutputs: vi.fn<typeof api.listOutputs>(),
    getRecordedRun: vi.fn<typeof api.getRecordedRun>(),
    createBatch: vi.fn<typeof api.createBatch>(),
    rerunRun: vi.fn<typeof api.rerunRun>(),
  },
  listeners: new Map<string, (event: EventEnvelope) => void>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake.client,
}));
vi.mock('../../lib/api/events', () => ({
  events: {
    status: writable<ConnectionStatus>('open'),
    resync: writable<ResyncSignal | null>(null),
    on(type: string, listener: (event: EventEnvelope) => void) {
      fake.listeners.set(type, listener);
      return () => fake.listeners.delete(type);
    },
  },
}));

const { client } = fake;
beforeEach(() => {
  client.getBatch.mockResolvedValue(
    batch({
      state: 'submitted',
      items: [
        item(0, {
          submission: 'submitted',
          run_id: 'run-0',
          run_status: 'queued',
        }),
      ],
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
const show = (id = 'batch-1') => render(BatchView, { id });

test('submission states never use the language of success', () => {
  for (const { label, detail } of Object.values(stateLabels))
    expect(`${label} ${detail}`).not.toMatch(/success|complete|done|finished/i);
});

test('a fully sent batch is described as sent, not as generated', async () => {
  show();
  expect(await screen.findByText(/All runs sent/)).toBeTruthy();
  expect(screen.getByText(/not the generation result/)).toBeTruthy();
  expect(screen.getByText('Queued')).toBeTruthy();
  expect(screen.queryByText('Completed')).toBeNull();
});

test('a run is shown as completed only from the backend run status', async () => {
  client.getBatch.mockResolvedValue(
    batch({
      state: 'submitted',
      items: [
        item(0, {
          submission: 'submitted',
          run_id: 'run-0',
          run_status: 'completed',
        }),
        item(1, { submission: 'submitted', run_id: 'run-1', run_status: null }),
      ],
    }),
  );
  show();
  await screen.findByText('Completed');
  expect(screen.getAllByText('Completed')).toHaveLength(1);
  expect(screen.getByText('No run yet')).toBeTruthy();
});

test('resume is offered for an interrupted batch and sends no new run', async () => {
  client.getBatch.mockResolvedValue(
    batch({
      state: 'interrupted',
      items: [
        item(0, {
          submission: 'submitted',
          run_id: 'run-0',
          run_status: 'running',
        }),
        item(1, { submission: 'not_submitted' }),
      ],
    }),
  );
  client.resumeBatch.mockResolvedValue(
    batch({
      state: 'submitting',
      items: [item(0, { submission: 'submitted', run_id: 'run-0' }), item(1)],
    }),
  );
  show();
  await userEvent.click(
    await screen.findByRole('button', { name: 'Resume submission' }),
  );
  expect(client.resumeBatch).toHaveBeenCalledWith('batch-1');
  expect(await screen.findByText(/Resumed/)).toBeTruthy();
  expect(client.createBatch).not.toHaveBeenCalled();
  expect(client.rerunRun).not.toHaveBeenCalled();
});

test('resume is not offered when nothing can be resumed', async () => {
  show();
  await screen.findByText(/All runs sent/);
  expect(screen.queryByRole('button', { name: /Resume/ })).toBeNull();
});

test('resume is not offered while the batch is submitting', async () => {
  client.getBatch.mockResolvedValue(
    batch({ state: 'submitting', items: [item(0), item(1)] }),
  );
  show();
  await screen.findByText(/Submitting runs/);
  expect(screen.queryByRole('button', { name: /Resume/ })).toBeNull();
});

test('a rejected item without a run can be resumed, but one with a run cannot', async () => {
  client.getBatch.mockResolvedValue(
    batch({
      state: 'partial',
      items: [
        item(0, {
          submission: 'failed',
          run_id: 'run-0',
          run_status: 'failed',
          error: 'Preparation failed.',
        }),
        item(1, {
          submission: 'submitted',
          run_id: 'run-1',
          run_status: 'queued',
        }),
      ],
    }),
  );
  show();
  await screen.findByText(/Some runs not sent/);
  expect(screen.queryByRole('button', { name: /Resume/ })).toBeNull();
  expect(screen.getByText('Preparation failed.')).toBeTruthy();
  expect(screen.queryByText('(can be resumed)')).toBeNull();
});

test('an unknown outcome links to reconciliation and offers no resubmission', async () => {
  client.getBatch.mockResolvedValue(
    batch({
      state: 'partial',
      items: [
        item(0, {
          submission: 'outcome_unknown',
          run_id: 'run-0',
          run_status: 'unknown_submission',
        }),
      ],
    }),
  );
  show();
  const link = await screen.findByRole('link', { name: 'Reconcile in Jobs' });
  expect(link.getAttribute('href')).toBe(
    '#/jobs?batch=batch-1&status=unknown_submission',
  );
  expect(
    screen.queryByRole('button', { name: /Resume|Submit|again/i }),
  ).toBeNull();
  expect(screen.getByText('Outcome unknown', { selector: 'td' })).toBeTruthy();
});

test('a run event for this batch refreshes the rows, other batches do not', async () => {
  show();
  await screen.findByText(/All runs sent/);
  expect(client.getBatch).toHaveBeenCalledTimes(1);
  const listener = fake.listeners.get('run_completed')!;
  listener({ type: 'run_completed', run: { batch_id: 'other' } } as never);
  await new Promise((resolve) => setTimeout(resolve, 600));
  expect(client.getBatch).toHaveBeenCalledTimes(1);
  listener({ type: 'run_completed', run: { batch_id: 'batch-1' } } as never);
  await waitFor(() => expect(client.getBatch).toHaveBeenCalledTimes(2), {
    timeout: 2000,
  });
});

test('a missing batch shows the problem and a reload button', async () => {
  client.getBatch.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Not found',
      status: 404,
      detail: 'No such batch.',
      recovery: 'Check the ID.',
    } as never),
  );
  show();
  expect(await screen.findByText('No such batch.')).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Reload batch' })).toBeTruthy();
});

test('polling stops when the view is closed', async () => {
  const { unmount } = show();
  await screen.findByText(/All runs sent/);
  unmount();
  expect(fake.listeners.size).toBe(0);
});

test('the run table is paginated', async () => {
  const items = Array.from({ length: 120 }, (_, i) =>
    item(i, {
      submission: 'submitted',
      run_id: `run-${i}`,
      run_status: 'queued',
    }),
  );
  client.getBatch.mockResolvedValue(batch({ state: 'submitted', items }));
  show();
  await screen.findByText(/Runs 1 to 50 of 120/);
  expect(within(screen.getByRole('table')).getAllByRole('row')).toHaveLength(
    51,
  );
});
