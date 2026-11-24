import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import { writable } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiProblemError } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import Page from './Page.svelte';

const fake = vi.hoisted(() => ({
  health: vi.fn(),
  listRuns: vi.fn(),
  listOutputs: vi.fn(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));
vi.mock('../../lib/api/events', async () => {
  const { writable } = await import('svelte/store');
  const handlers = new Map<string, Set<(event: never) => void>>();
  return {
    events: {
      status: writable('connecting'),
      resync: writable(null),
      on(type: string, handler: (event: never) => void) {
        if (!handlers.has(type)) handlers.set(type, new Set());
        handlers.get(type)!.add(handler);
        return () => handlers.get(type)!.delete(handler);
      },
    },
    handlers,
  };
});
import { events } from '../../lib/api/events';
import * as eventModule from '../../lib/api/events';

const handlers = (
  eventModule as unknown as {
    handlers: Map<string, Set<(event: never) => void>>;
  }
).handlers;
const status = events.status as ReturnType<typeof writable<string>>;
const resync = events.resync as ReturnType<typeof writable<unknown>>;
const healthy = {
  self: 'ok',
  image_backend: 'ok',
  llm: 'not_configured',
  image_backend_reason: null,
};
const makeRun = (
  id: string,
  status: components['schemas']['RunStatus'],
  updated_at = '2026-01-01T10:00:00Z',
) => ({
  id,
  status,
  updated_at,
  created_at: '2026-01-01T09:00:00Z',
  prompt_id: 'p',
  workflow_id: `workflow-${id}`,
  workflow_version: '1',
  workflow_sha256: 'hash',
  parameters: {},
});
function emit(type: string, data: object) {
  for (const handler of handlers.get(type) ?? [])
    handler({ type, ...data } as never);
}
function mount() {
  return render(Page, { params: {}, query: new URLSearchParams() });
}

beforeEach(() => {
  fake.health.mockReset().mockResolvedValue(healthy);
  fake.listRuns.mockReset().mockResolvedValue({ runs: [], next_cursor: null });
  fake.listOutputs
    .mockReset()
    .mockResolvedValue({ outputs: [], next_cursor: null });
  status.set('connecting');
  resync.set(null);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('dashboard', () => {
  it('shows health and updates it from a probe event, with connection states', async () => {
    mount();
    expect(screen.getByText('Connecting to live updates…')).toBeTruthy();
    await screen.findByText('Not configured');
    const cards = screen.getAllByRole('article');
    expect(within(cards[0]).getByText('OK')).toBeTruthy();
    expect(within(cards[1]).getByText('OK')).toBeTruthy();
    expect(within(cards[2]).getByText('Not configured')).toBeTruthy();
    status.set('open');
    await waitFor(() => expect(fake.health).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.getByText('Live updates connected')).toBeTruthy(),
    );
    fake.health.mockResolvedValue({
      ...healthy,
      image_backend: 'unavailable',
      image_backend_reason: 'timeout',
    });
    emit('backend_health_changed', {
      backend: 'comfyui',
      health: null,
      error: 'Timed out',
    });
    await screen.findByText('The health probe timed out.');
    expect(within(cards[1]).getByText('Unavailable')).toBeTruthy();
    status.set('reconnecting');
    expect(await screen.findByText(/Live updates offline/)).toBeTruthy();
  });
});

it('merges live runs into the snapshot and links unknown submissions to reconciliation', async () => {
  fake.listRuns.mockResolvedValue({
    runs: [makeRun('a', 'queued')],
    next_cursor: null,
  });
  mount();
  expect(
    (await screen.findByRole('link', { name: 'workflow-a' })).getAttribute(
      'href',
    ),
  ).toBe('#/runs/a');
  emit('run_unknown_submission', {
    run: makeRun('a', 'unknown_submission', '2026-01-01T10:01:00Z'),
  });
  await screen.findByText(/Unknown submissions need reconciliation/);
  expect(
    screen.getByText('Needs reconciliation', { selector: 'strong' }),
  ).toBeTruthy();
  expect(
    screen.getByRole('link', { name: 'Go to Jobs' }).getAttribute('href'),
  ).toBe('#/jobs');
  emit('run_completed', {
    run: makeRun('a', 'completed', '2026-01-01T10:02:00Z'),
  });
  expect(
    await screen.findByText('Completed', { selector: 'strong' }),
  ).toBeTruthy();
  expect(
    screen.queryByText(/Unknown submissions need reconciliation/),
  ).toBeNull();
});

it('shows empty states and problem details with the API recovery hint', async () => {
  mount();
  await screen.findByText(/No runs yet/);
  expect(screen.getByText('No outputs yet.')).toBeTruthy();
  cleanup();
  fake.health.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Health failed',
      status: 503,
      detail: 'Service unavailable.',
      recovery: 'Restart the local service.',
    }),
  );
  fake.listRuns.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Runs failed',
      status: 503,
      detail: 'Database unavailable.',
      recovery: 'Check the database.',
    }),
  );
  mount();
  await screen.findByText('Health failed');
  expect(screen.getByText('Restart the local service.')).toBeTruthy();
  await screen.findByText('Runs failed');
  expect(screen.getByText('Check the database.')).toBeTruthy();
  expect(screen.queryByText(/No runs yet/)).toBeNull();
});

it('buffers live runs during paginated snapshots and resynchronises all sections', async () => {
  let resolvePage!: (value: unknown) => void;
  fake.listRuns
    .mockResolvedValueOnce({
      runs: [makeRun('a', 'queued')],
      next_cursor: 'next',
    })
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolvePage = resolve;
        }),
    )
    .mockResolvedValue({ runs: [], next_cursor: null });
  mount();
  await waitFor(() => expect(resolvePage).toBeDefined());
  emit('run_started', {
    run: makeRun('a', 'running', '2026-01-01T10:00:00.000000001Z'),
  });
  emit('run_failed', { run: makeRun('b', 'failed', '2026-01-01T09:59:00Z') });
  resolvePage({ runs: [makeRun('b', 'completed')], next_cursor: null });
  expect(
    await screen.findByText('Running', { selector: 'strong' }),
  ).toBeTruthy();
  expect(screen.getByText('Completed', { selector: 'strong' })).toBeTruthy();
  expect(fake.listRuns.mock.calls[1][0]).toEqual({
    limit: 200,
    cursor: 'next',
  });
  const calls = [
    fake.health.mock.calls.length,
    fake.listRuns.mock.calls.length,
    fake.listOutputs.mock.calls.length,
  ];
  resync.set({ epoch: 1, reason: { reason: 'lagged', skipped: 1 } });
  await waitFor(() =>
    expect(fake.health.mock.calls.length).toBeGreaterThan(calls[0]),
  );
  await waitFor(() =>
    expect(fake.listRuns.mock.calls.length).toBeGreaterThan(calls[1]),
  );
  await waitFor(() =>
    expect(fake.listOutputs.mock.calls.length).toBeGreaterThan(calls[2]),
  );
});

it('shows recent output thumbnails using API URLs', async () => {
  fake.listOutputs.mockResolvedValue({
    outputs: [{ id: 'output/1', source_filename: 'image.png' }],
    next_cursor: null,
  });
  mount();
  const link = await screen.findByRole('link', {
    name: 'View image.png in Gallery',
  });
  expect(link.getAttribute('href')).toBe('#/gallery');
  expect(within(link).getByRole('presentation').getAttribute('src')).toBe(
    '/api/v1/outputs/output%2F1/thumbnail?size=256',
  );
});

it('polls health only while visible and cleans up subscriptions', async () => {
  vi.useFakeTimers();
  mount();
  await vi.waitFor(() => expect(fake.health).toHaveBeenCalledTimes(1));
  Object.defineProperty(document, 'hidden', {
    configurable: true,
    value: true,
  });
  await vi.advanceTimersByTimeAsync(30_000);
  expect(fake.health).toHaveBeenCalledTimes(1);
  Object.defineProperty(document, 'hidden', {
    configurable: true,
    value: false,
  });
  document.dispatchEvent(new Event('visibilitychange'));
  await vi.waitFor(() => expect(fake.health).toHaveBeenCalledTimes(2));
  cleanup();
  expect([...handlers.values()].every((set) => set.size === 0)).toBe(true);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(fake.health).toHaveBeenCalledTimes(2);
  Reflect.deleteProperty(document, 'hidden');
});

it('does not start another probe from its own in-flight health event, but resync replaces it', async () => {
  let finish!: (value: unknown) => void;
  fake.health
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue(healthy);
  mount();
  await waitFor(() => expect(finish).toBeDefined());
  emit('backend_health_changed', {
    backend: 'comfyui',
    health: {},
    error: null,
  });
  expect(fake.health).toHaveBeenCalledTimes(1);
  const oldSignal = fake.health.mock.calls[0][0].signal as AbortSignal;
  resync.set({ epoch: 1, reason: { reason: 'lagged', skipped: 1 } });
  await waitFor(() => expect(fake.health).toHaveBeenCalledTimes(2));
  expect(oldSignal.aborted).toBe(true);
  await screen.findByText('Not configured');
  finish({ ...healthy, image_backend: 'unavailable' });
  await Promise.resolve();
  expect(screen.queryByText('Unavailable')).toBeNull();
});
