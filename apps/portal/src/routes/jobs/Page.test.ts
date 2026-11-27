import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { writable } from 'svelte/store';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import {
  ApiNetworkError,
  ApiProblemError,
  type api,
} from '../../lib/api/client';
import type {
  ConnectionStatus,
  EventEnvelope,
  ResyncSignal,
} from '../../lib/api/events';
import { mockNativeDialog } from '../../test/dialog';
import Page from './Page.svelte';
import type { Run } from './run-model';

const fake = vi.hoisted(() => ({
  client: {
    listRuns: vi.fn<typeof api.listRuns>(),
    getRun: vi.fn<typeof api.getRun>(),
    cancelRun: vi.fn<typeof api.cancelRun>(),
    reconcileRun: vi.fn<typeof api.reconcileRun>(),
    rerunRun: vi.fn<(id: string, key: string) => Promise<Run>>(),
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

import { events } from '../../lib/api/events';

const stamp = '2026-11-24T12:00:00.123456Z';
const original: Run = {
  id: 'run-example',
  prompt_id: 'prompt-1',
  workflow_id: 'example',
  workflow_version: '1',
  workflow_sha256: 'hash',
  parameters: { seed: 123, prompt: 'A lighthouse' },
  status: 'running',
  created_at: stamp,
  updated_at: stamp,
};
const eventRun = {
  ...original,
  client_id: 'client',
  generation_mode: 'generic' as const,
  output_dir: 'output',
};
const envelope = { id: 1, bus_id: 'bus', timestamp: stamp };
const { client } = fake;
const setResync = (epoch: number) =>
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch,
    reason: { reason: 'cursor_unavailable' },
  });

function start(run = original, query = '') {
  client.listRuns.mockResolvedValue({ runs: [run] });
  render(Page, { query: new URLSearchParams(query), params: {} });
  return screen.findByRole('link', { name: run.id });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function emit(event: EventEnvelope) {
  fake.listeners.get(event.type)?.(event);
}
function progress(value: number, run = eventRun) {
  emit({
    ...envelope,
    type: 'run_progress',
    run,
    backend_event: {
      event_type: 'progress',
      value,
      maximum: 10,
      node_id: '7',
      raw: null,
    },
  });
}

mockNativeDialog();
beforeEach(() => vi.resetAllMocks());
afterEach(() => {
  cleanup();
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set(null);
  window.history.replaceState(null, '', '/');
});

test('shows live node and progress, and resets when execution moves to another node', async () => {
  const link = await start();
  expect(link.getAttribute('href')).toBe('#/runs/run-example');
  progress(3);
  expect(await screen.findByText('3 of 10')).toBeTruthy();
  expect(screen.getByText('7')).toBeTruthy();
  emit({
    ...envelope,
    type: 'run_progress',
    run: eventRun,
    backend_event: { event_type: 'executing', node_id: '9', raw: null },
  });
  expect(await screen.findByText('9')).toBeTruthy();
  expect(screen.queryByRole('progressbar')).toBeNull();
});

test('refetches on resync and applies newer buffered progress without regressing the snapshot', async () => {
  await start();
  const response = deferred<Awaited<ReturnType<typeof api.listRuns>>>();
  client.listRuns.mockReturnValueOnce(response.promise);
  setResync(1);
  expect(client.listRuns).toHaveBeenCalledTimes(2);
  const newer = { ...eventRun, updated_at: '2026-11-24T12:00:00.123457Z' };
  progress(6, newer);
  progress(2, eventRun);
  expect(screen.queryByRole('progressbar')).toBeNull();
  response.resolve({ runs: [newer] });
  expect(await screen.findByText('6 of 10')).toBeTruthy();
  expect(screen.queryByText('2 of 10')).toBeNull();
});

test('loads filters and cursor from the hash query and resets paging when filters change', async () => {
  await start(original, 'status=running&workflow=example&cursor=page');
  expect(client.listRuns).toHaveBeenCalledWith(
    { status: 'running', workflow: 'example', cursor: 'page', limit: 25 },
    expect.anything(),
  );
  await fireEvent.input(screen.getByRole('textbox', { name: 'Workflow ID' }), {
    target: { value: 'other workflow' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Apply filters' }));
  expect(window.location.hash).toBe(
    '#/jobs?status=running&workflow=other+workflow',
  );
});

test('confirms cancellation, disables the action in flight and shows the backend result', async () => {
  const user = userEvent.setup();
  await start();
  const response = deferred<Run>();
  client.cancelRun.mockReturnValueOnce(response.promise);
  await user.click(screen.getByRole('button', { name: 'Cancel run' }));
  expect(client.cancelRun).not.toHaveBeenCalled();
  await user.click(
    within(screen.getByRole('dialog')).getByRole('button', {
      name: 'Confirm cancellation',
    }),
  );
  expect(client.cancelRun).toHaveBeenCalledExactlyOnceWith(original.id);
  expect(
    (screen.getByRole('button', { name: 'Cancel run' }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  const cancelled = {
    ...original,
    status: 'cancelled' as const,
    updated_at: '2026-11-24T12:00:01Z',
  };
  client.listRuns.mockResolvedValue({ runs: [cancelled] });
  response.resolve(cancelled);
  expect(await screen.findByText('Run cancelled.')).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Cancel run' })).toBeNull();
});

test('shows the cancellation problem and its recovery hint together', async () => {
  await start();
  client.cancelRun.mockRejectedValueOnce(
    new ApiProblemError({
      status: 409,
      type: 'cancel-uncertain',
      title: 'Cancellation uncertain',
      detail: 'ComfyUI did not acknowledge the request.',
      recovery: 'Reconcile the run before taking further action.',
    }),
  );
  await userEvent.click(screen.getByRole('button', { name: 'Cancel run' }));
  await userEvent.click(
    screen.getByRole('button', { name: 'Confirm cancellation' }),
  );
  const alert = await screen.findByRole('alert');
  expect(within(alert).getByText('Cancellation uncertain')).toBeTruthy();
  expect(alert.textContent).toContain(
    'Reconcile the run before taking further action.',
  );
});

test('requires a not-found outcome and explicit review before creating a new run with a fresh key', async () => {
  const unknown = { ...original, status: 'unknown_submission' as const };
  await start(unknown);
  expect(
    screen.queryByRole('button', { name: 'Submit as new run' }),
  ).toBeNull();
  client.reconcileRun.mockResolvedValueOnce({
    ...unknown,
    outcome: 'still_ambiguous',
    settles_at: '2026-11-24T12:01:10Z',
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  expect(await screen.findByText(/Reconcile again at or after/)).toBeTruthy();
  expect(
    screen.queryByRole('button', { name: 'Submit as new run' }),
  ).toBeNull();

  client.reconcileRun.mockResolvedValueOnce({
    ...unknown,
    outcome: 'not_found',
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  const submit = await screen.findByRole('button', {
    name: 'Submit as new run',
  });
  await userEvent.click(submit);
  expect(screen.getByRole('dialog').textContent).toContain('A lighthouse');
  expect(screen.getByRole('dialog').textContent).toContain('could duplicate');
  expect(client.rerunRun).not.toHaveBeenCalled();
  client.getRun.mockResolvedValueOnce(unknown);
  const newRun = { ...original, id: 'run-new', status: 'queued' as const };
  client.rerunRun.mockResolvedValueOnce(newRun);
  await userEvent.click(
    screen.getByRole('button', { name: 'Confirm new run' }),
  );
  expect(
    (await screen.findByRole('link', { name: 'View new run' })).getAttribute(
      'href',
    ),
  ).toBe('#/runs/run-new');
  expect(client.rerunRun).toHaveBeenCalledExactlyOnceWith(
    original.id,
    expect.stringMatching(/^[a-f0-9-]{36}$/),
  );
  expect(client.rerunRun.mock.calls[0][1]).not.toBe(original.prompt_id);
  expect(client.cancelRun).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      screen.queryByRole('button', { name: 'Submit as new run' }),
    ).toBeNull(),
  );
});

test('a batch run offers its re-run as outside the batch', async () => {
  const unknown = {
    ...original,
    batch_id: 'batch-7',
    status: 'unknown_submission' as const,
  };
  await start(unknown);
  client.reconcileRun.mockResolvedValueOnce({
    ...unknown,
    outcome: 'not_found',
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  expect(
    await screen.findByRole('button', {
      name: 'Submit as new run outside this batch',
    }),
  ).toBeTruthy();
  expect(
    screen.getByText('The new run will not appear in this batch.'),
  ).toBeTruthy();
  expect(
    screen.queryByRole('button', { name: 'Submit as new run' }),
  ).toBeNull();
});

function rerunProblem(status: number, type = 'about:blank') {
  return new ApiProblemError({
    status,
    type,
    title: 'Re-run error',
    detail: 'Could not re-run.',
    recovery: 'Check Jobs.',
  });
}

async function rejectRerun(error: Error) {
  const unknown = { ...original, status: 'unknown_submission' as const };
  await start(unknown);
  client.reconcileRun.mockResolvedValue({ ...unknown, outcome: 'not_found' });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  await userEvent.click(
    await screen.findByRole('button', { name: 'Submit as new run' }),
  );
  client.getRun.mockResolvedValue(unknown);
  client.rerunRun.mockRejectedValueOnce(error);
  await userEvent.click(
    screen.getByRole('button', { name: 'Confirm new run' }),
  );
  return unknown;
}

test.each([
  [
    'fingerprint mismatch',
    rerunProblem(409, 'urn:genaimage:problem:rerun-mismatch'),
  ],
  ['definitive rejection', rerunProblem(422)],
])(
  'allows another confirmed attempt after fresh reconciliation on %s',
  async (_label, error) => {
    const unknown = await rejectRerun(error);
    await screen.findByText(/No new run was created/);
    expect(
      screen.queryByRole('button', { name: 'Submit as new run' }),
    ).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
    await userEvent.click(
      await screen.findByRole('button', { name: 'Submit as new run' }),
    );
    expect(client.rerunRun).toHaveBeenCalledTimes(1);
    client.rerunRun.mockResolvedValueOnce({
      ...unknown,
      id: 'run-new',
      status: 'queued',
    });
    await userEvent.click(
      screen.getByRole('button', { name: 'Confirm new run' }),
    );
    await screen.findByRole('link', { name: 'View new run' });
    expect(client.rerunRun).toHaveBeenCalledTimes(2);
    expect(client.rerunRun.mock.calls[1][1]).not.toBe(
      client.rerunRun.mock.calls[0][1],
    );
  },
);

test.each([
  [
    'network failure',
    new ApiNetworkError(
      'Could not verify the request.',
      new Error('Connection lost'),
    ),
  ],
  ['timeout', rerunProblem(408)],
  ['server failure', rerunProblem(502)],
])(
  'keeps a new submission blocked after an ambiguous %s',
  async (_label, error) => {
    await rejectRerun(error);
    await screen.findByText(/A new run was not confirmed/);
    await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
    await screen.findByText(/Reconciliation did not find the prompt/);
    expect(
      screen.queryByRole('button', { name: 'Submit as new run' }),
    ).toBeNull();
    expect(client.rerunRun).toHaveBeenCalledTimes(1);
  },
);

test('refetches for an unseen matching progress revision and coalesces repeats', async () => {
  const query = new URLSearchParams('status=running');
  client.listRuns.mockResolvedValue({ runs: [] });
  render(Page, { query, params: {} });
  await screen.findByText(/No runs match/);
  progress(1);
  progress(2);
  await waitFor(() => expect(client.listRuns).toHaveBeenCalledTimes(2));
  progress(3);
  const revised = { ...eventRun, updated_at: '2026-11-24T12:00:01Z' };
  client.listRuns.mockResolvedValue({ runs: [revised] });
  progress(4, revised);
  progress(5, revised);
  await screen.findByRole('link', { name: original.id });
  expect(client.listRuns).toHaveBeenCalledTimes(3);
});

test('does not portray an unrecognised status as successful', async () => {
  await start({ ...original, status: 'successful' as Run['status'] });
  expect(screen.getByText('Unknown status')).toBeTruthy();
  expect(screen.queryByText('Completed')).toBeTruthy(); // Only the filter option.
  expect(
    within(screen.getByRole('list', { name: 'Runs' })).queryByText('Completed'),
  ).toBeNull();
});

test('the batch filter asks the server for that batch and rows link to it', async () => {
  const member = {
    ...original,
    id: 'run-member',
    batch_id: 'batch-7',
    batch_index: 2,
  };
  await start(member, 'batch=batch-7');
  expect(client.listRuns).toHaveBeenCalledWith(
    expect.objectContaining({ batch: 'batch-7' }),
    expect.anything(),
  );
  expect(screen.getByLabelText('Batch ID')).toHaveProperty('value', 'batch-7');
  expect(
    screen.getByRole('link', { name: 'Batch run 3' }).getAttribute('href'),
  ).toBe('#/batches/batch-7');
});

test('an event for a run outside the batch filter is not added', async () => {
  await start(
    { ...original, batch_id: 'batch-7', batch_index: 0 },
    'batch=batch-7',
  );
  client.listRuns.mockClear();
  emit({
    ...envelope,
    type: 'run_queued',
    run: { ...eventRun, id: 'run-other', batch_id: 'batch-8' },
  } as never);
  await new Promise((resolve) => setTimeout(resolve, 250));
  expect(client.listRuns).not.toHaveBeenCalled();
});
