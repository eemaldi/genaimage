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
import { ApiProblemError, type api } from '../../lib/api/client';
import type {
  ConnectionStatus,
  EventEnvelope,
  ResyncSignal,
} from '../../lib/api/events';
import { mockNativeDialog } from '../../test/dialog';
import Page from './Page.svelte';
import type { ReconciledRun, Run } from './run-model';

const fake = vi.hoisted(() => ({
  client: {
    listRuns: vi.fn<typeof api.listRuns>(),
    getRun: vi.fn<typeof api.getRun>(),
    getWorkflow: vi.fn<typeof api.getWorkflow>(),
    cancelRun: vi.fn<typeof api.cancelRun>(),
    reconcileRun: vi.fn<(id: string) => Promise<ReconciledRun>>(),
    createRun: vi.fn<typeof api.createRun>(),
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
  client.reconcileRun.mockResolvedValueOnce(unknown);
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  expect(await screen.findByText(/still ambiguous/)).toBeTruthy();
  expect(
    screen.queryByRole('button', { name: 'Submit as new run' }),
  ).toBeNull();

  client.reconcileRun.mockResolvedValueOnce({
    ...unknown,
    reconciliation_outcome: 'not_found',
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  const submit = await screen.findByRole('button', {
    name: 'Submit as new run',
  });
  await userEvent.click(submit);
  expect(screen.getByRole('dialog').textContent).toContain('A lighthouse');
  expect(client.createRun).not.toHaveBeenCalled();
  client.getRun.mockResolvedValueOnce(unknown);
  client.getWorkflow.mockResolvedValueOnce({
    id: 'example',
    title: 'Example',
    version: '1',
    workflow: 'workflow.json',
    outputs: [],
    parameters: {},
    presets: {},
    schema_version: 2,
  });
  const newRun = { ...original, id: 'run-new', status: 'queued' as const };
  client.createRun.mockResolvedValueOnce(newRun);
  await userEvent.click(
    screen.getByRole('button', { name: 'Confirm new run' }),
  );
  expect(
    (await screen.findByRole('link', { name: 'View new run' })).getAttribute(
      'href',
    ),
  ).toBe('#/runs/run-new');
  expect(client.createRun).toHaveBeenCalledExactlyOnceWith(
    { workflow: 'example', parameters: original.parameters },
    expect.stringMatching(/^[a-f0-9-]{36}$/),
  );
  expect(client.createRun.mock.calls[0][1]).not.toBe(original.prompt_id);
  expect(client.cancelRun).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      screen.queryByRole('button', { name: 'Submit as new run' }),
    ).toBeNull(),
  );
});

test('links to Generate when a not-found run lacks reusable parameters', async () => {
  const unknown = {
    ...original,
    status: 'unknown_submission' as const,
    parameters: null,
  };
  await start(unknown);
  client.reconcileRun.mockResolvedValueOnce({
    ...unknown,
    reconciliation_outcome: 'not_found',
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reconcile' }));
  expect(
    (await screen.findByRole('link', { name: 'Open Generate' })).getAttribute(
      'href',
    ),
  ).toBe('#/generate?workflow=example');
  expect(
    screen.queryByRole('button', { name: 'Submit as new run' }),
  ).toBeNull();
});

test('does not portray an unrecognised status as successful', async () => {
  await start({ ...original, status: 'successful' as Run['status'] });
  expect(screen.getByText('Unknown status')).toBeTruthy();
  expect(screen.queryByText('Completed')).toBeTruthy(); // Only the filter option.
  expect(
    within(screen.getByRole('list', { name: 'Runs' })).queryByText('Completed'),
  ).toBeNull();
});
