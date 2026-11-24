import {
  cleanup,
  fireEvent,
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
  hints: {
    self: 'Start GenAImage.',
    image_backend: 'Start ComfyUI and check Settings.',
    llm: 'LLM integration is planned.',
  },
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
    expect(
      within(cards[2]).getByText('LLM integration is planned.'),
    ).toBeTruthy();
    status.set('open');
    await waitFor(() => expect(fake.health).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.getByText('Live updates connected')).toBeTruthy(),
    );
    vi.useFakeTimers();
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
    await vi.advanceTimersByTimeAsync(5_001);
    expect(screen.getByText('The health probe timed out.')).toBeTruthy();
    expect(
      within(cards[1]).getByText('Start ComfyUI and check Settings.'),
    ).toBeTruthy();
    expect(within(cards[1]).getByText('Unavailable')).toBeTruthy();
    status.set('reconnecting');
    await vi.waitFor(() =>
      expect(screen.getByText(/Live updates offline/)).toBeTruthy(),
    );
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
    (
      await screen.findByText('Run a needs reconciliation. Go to Jobs.')
    ).getAttribute('aria-live'),
  ).toBe('polite');
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

it('buffers live runs during snapshots and resynchronises all sections', async () => {
  let resolvePage!: (value: unknown) => void;
  fake.listRuns
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
  resolvePage({
    runs: [makeRun('a', 'queued'), makeRun('b', 'completed')],
    next_cursor: null,
  });
  expect(
    await screen.findByText('Running', { selector: 'strong' }),
  ).toBeTruthy();
  expect(screen.getByText('Completed', { selector: 'strong' })).toBeTruthy();
  expect(fake.listRuns.mock.calls[0][0]).toEqual({ limit: 50 });
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

it('caps snapshots and live state and labels partial counts as lower bounds', async () => {
  fake.listRuns.mockResolvedValue({
    runs: Array.from({ length: 50 }, (_, i) =>
      makeRun(String(i).padStart(2, '0'), 'queued'),
    ),
    next_cursor: 'older',
  });
  mount();
  await screen.findByText('50+');
  expect(fake.listRuns).toHaveBeenCalledTimes(1);
  expect(fake.listRuns.mock.calls[0][0]).toEqual({ limit: 50 });
  for (let i = 0; i < 10; i++)
    emit('run_started', { run: makeRun(`z${i}`, 'running') });
  await waitFor(() => expect(screen.getByText('40+')).toBeTruthy());
  expect(screen.getByText('10+')).toBeTruthy();
  expect(screen.getAllByRole('link', { name: /workflow-/ })).toHaveLength(6);
});

it('links each output and falls back for unsupported or broken thumbnails', async () => {
  fake.listOutputs.mockResolvedValue({
    outputs: [
      {
        id: 'output/1',
        source_filename: 'image.png',
        content_type: 'image/png',
      },
      {
        id: 'output 2',
        source_filename: 'film.mp4',
        content_type: 'video/mp4',
      },
    ],
    next_cursor: null,
  });
  mount();
  const image = await screen.findByRole('link', {
    name: 'View image.png in Gallery',
  });
  const video = screen.getByRole('link', { name: 'View film.mp4 in Gallery' });
  expect(image.getAttribute('href')).toBe('#/gallery?output=output%2F1');
  expect(video.getAttribute('href')).toBe('#/gallery?output=output%202');
  expect(
    within(video).getByText('Thumbnail unavailable (video/mp4)'),
  ).toBeTruthy();
  expect(within(video).queryByRole('presentation')).toBeNull();
  const thumbnail = within(image).getByRole('presentation');
  expect(thumbnail.getAttribute('src')).toBe(
    '/api/v1/outputs/output%2F1/thumbnail?size=256',
  );
  await fireEvent.error(thumbnail);
  expect(
    within(image).getByText('Thumbnail unavailable (image/png)'),
  ).toBeTruthy();
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

it('coalesces a health event during a probe and reconciles a newer observation', async () => {
  vi.useFakeTimers();
  let finish!: (value: unknown) => void;
  fake.health
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue({
      ...healthy,
      image_backend: 'unavailable',
      image_backend_reason: 'probe_failed',
    });
  mount();
  await vi.waitFor(() => expect(finish).toBeDefined());
  emit('backend_health_changed', {
    backend: 'ComfyUI',
    health: null,
    error: 'Unavailable',
  });
  emit('backend_health_changed', {
    backend: 'ComfyUI',
    health: null,
    error: 'Unavailable',
  });
  finish(healthy);
  await vi.advanceTimersByTimeAsync(4_000);
  expect(fake.health).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(fake.health).toHaveBeenCalledTimes(2);
  await vi.waitFor(() => expect(screen.getByText('Unavailable')).toBeTruthy());
});

it('does not re-probe for the health event delivered after its matching response', async () => {
  vi.useFakeTimers();
  mount();
  await vi.waitFor(() =>
    expect(screen.getByText('Not configured')).toBeTruthy(),
  );
  emit('backend_health_changed', {
    backend: 'ComfyUI',
    health: { system: {} },
    error: null,
  });
  await vi.advanceTimersByTimeAsync(10_000);
  expect(fake.health).toHaveBeenCalledTimes(1);
});

it.each([
  ['run_completed', 'completed'],
  ['run_failed', 'failed'],
  ['run_cancelled', 'cancelled'],
] as const)(
  'announces %s transitions but not initial state or progress',
  async (eventType, result) => {
    fake.listRuns.mockResolvedValue({
      runs: [makeRun('a', 'queued'), makeRun('old', result)],
      next_cursor: null,
    });
    const { container } = mount();
    await screen.findByRole('link', { name: 'workflow-a' });
    const live = container.querySelector('[aria-live="polite"]')!;
    expect(live.textContent?.trim()).toBe('');
    vi.useFakeTimers();
    emit('run_progress', {
      run: makeRun('a', 'running', '2026-01-01T10:01:00Z'),
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(live.textContent?.trim()).toBe('');
    emit(eventType, { run: makeRun('a', result, '2026-01-01T10:02:00Z') });
    await vi.advanceTimersByTimeAsync(400);
    expect(live.textContent?.trim()).toBe(`Run a ${result}.`);
    emit(eventType, { run: makeRun('a', result, '2026-01-01T10:03:00Z') });
    await vi.advanceTimersByTimeAsync(500);
    expect(live.textContent?.trim()).toBe(`Run a ${result}.`);
  },
);

it('coalesces a burst of run transitions into one concise announcement', async () => {
  fake.listRuns.mockResolvedValue({
    runs: ['a', 'b', 'c'].map((id) => makeRun(id, 'queued')),
    next_cursor: null,
  });
  const { container } = mount();
  await screen.findByRole('link', { name: 'workflow-a' });
  const live = container.querySelector('[aria-live="polite"]')!;
  vi.useFakeTimers();
  emit('run_completed', {
    run: makeRun('a', 'completed', '2026-01-01T10:01:00Z'),
  });
  emit('run_failed', { run: makeRun('b', 'failed', '2026-01-01T10:01:00Z') });
  emit('run_failed', { run: makeRun('c', 'failed', '2026-01-01T10:01:00Z') });
  emit('run_cancelled', {
    run: makeRun('c', 'cancelled', '2026-01-01T10:02:00Z'),
  });
  await vi.advanceTimersByTimeAsync(399);
  expect(live.textContent?.trim()).toBe('');
  await vi.advanceTimersByTimeAsync(1);
  expect(live.textContent?.trim()).toBe(
    'Run a completed. Run b failed. 1 more update. Check Jobs and service health.',
  );
});

it('announces health transitions after initial load and coalesces service changes', async () => {
  const { container } = mount();
  await screen.findByText('Not configured');
  const live = container.querySelector('[aria-live="polite"]')!;
  expect(live.textContent?.trim()).toBe('');
  vi.useFakeTimers();
  fake.health.mockResolvedValue({
    ...healthy,
    image_backend: 'unavailable',
    image_backend_reason: 'probe_failed',
  });
  await fireEvent.click(screen.getByRole('button', { name: 'Refresh health' }));
  await vi.waitFor(() => expect(screen.getByText('Unavailable')).toBeTruthy());
  await vi.advanceTimersByTimeAsync(400);
  expect(live.textContent?.trim()).toBe('ComfyUI is unavailable.');
  fake.health.mockResolvedValue({ ...healthy, llm: 'ok' });
  await fireEvent.click(screen.getByRole('button', { name: 'Refresh health' }));
  await vi.waitFor(() => expect(screen.queryByText('Unavailable')).toBeNull());
  await vi.advanceTimersByTimeAsync(400);
  expect(live.textContent?.trim()).toBe(
    'ComfyUI is available again. LLM is available again.',
  );
});

it('announces status changes discovered on resync without announcing initial snapshots', async () => {
  fake.listRuns
    .mockResolvedValueOnce({
      runs: [makeRun('a', 'queued')],
      next_cursor: null,
    })
    .mockResolvedValue({
      runs: [makeRun('a', 'completed', '2026-01-01T10:01:00Z')],
      next_cursor: null,
    });
  const { container } = mount();
  await screen.findByRole('link', { name: 'workflow-a' });
  const live = container.querySelector('[aria-live="polite"]')!;
  expect(live.textContent?.trim()).toBe('');
  resync.set({ epoch: 1, reason: { reason: 'lagged', skipped: 1 } });
  expect(await screen.findByText('Run a completed.')).toBeTruthy();
});

it('cancels queued announcements when the dashboard unmounts', async () => {
  fake.listRuns.mockResolvedValue({
    runs: [makeRun('a', 'queued')],
    next_cursor: null,
  });
  mount();
  await screen.findByRole('link', { name: 'workflow-a' });
  vi.useFakeTimers();
  emit('run_failed', { run: makeRun('a', 'failed', '2026-01-01T10:01:00Z') });
  cleanup();
  expect(vi.getTimerCount()).toBe(0);
});

it('drops an obsolete reconciliation announcement when the run starts in the same burst', async () => {
  fake.listRuns.mockResolvedValue({
    runs: [makeRun('a', 'queued')],
    next_cursor: null,
  });
  const { container } = mount();
  await screen.findByRole('link', { name: 'workflow-a' });
  const live = container.querySelector('[aria-live="polite"]')!;
  vi.useFakeTimers();
  emit('run_unknown_submission', {
    run: makeRun('a', 'unknown_submission', '2026-01-01T10:01:00Z'),
  });
  emit('run_started', { run: makeRun('a', 'running', '2026-01-01T10:02:00Z') });
  await vi.advanceTimersByTimeAsync(500);
  expect(live.textContent?.trim()).toBe('');
});

it.each([
  ['run_completed', 'completed'],
  ['run_failed', 'failed'],
  ['run_cancelled', 'cancelled'],
] as const)(
  'announces terminal state in progress before %s without duplicate announcements',
  async (eventType, result) => {
    fake.listRuns.mockResolvedValue({
      runs: [makeRun('a', 'running')],
      next_cursor: null,
    });
    const { container } = mount();
    await screen.findByRole('link', { name: 'workflow-a' });
    const live = container.querySelector('[aria-live="polite"]')!;
    vi.useFakeTimers();
    const observe = vi.fn();
    const observer = new MutationObserver(observe);
    observer.observe(live, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    emit('run_progress', { run: makeRun('a', result, '2026-01-01T10:01:00Z') });
    emit(eventType, { run: makeRun('a', result, '2026-01-01T10:02:00Z') });
    await vi.advanceTimersByTimeAsync(400);
    expect(live.textContent?.trim()).toBe(`Run a ${result}.`);
    observe.mockClear();
    emit(eventType, { run: makeRun('a', result, '2026-01-01T10:03:00Z') });
    await vi.advanceTimersByTimeAsync(500);
    expect(observe).not.toHaveBeenCalled();
    observer.disconnect();
  },
);

it('announces new terminal runs from a resync snapshot and buffered events once', async () => {
  let finish!: (value: unknown) => void;
  fake.listRuns
    .mockResolvedValueOnce({ runs: [], next_cursor: null })
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue({
      runs: [makeRun('a', 'completed'), makeRun('b', 'failed')],
      next_cursor: null,
    });
  const { container } = mount();
  await screen.findByText(/No runs yet/);
  const live = container.querySelector('[aria-live="polite"]')!;
  expect(live.textContent?.trim()).toBe('');
  vi.useFakeTimers();
  const observe = vi.fn();
  const observer = new MutationObserver(observe);
  observer.observe(live, {
    childList: true,
    characterData: true,
    subtree: true,
  });
  resync.set({ epoch: 1, reason: { reason: 'lagged', skipped: 1 } });
  await vi.waitFor(() => expect(finish).toBeDefined());
  emit('run_progress', { run: makeRun('b', 'failed') });
  emit('run_failed', { run: makeRun('b', 'failed') });
  finish({ runs: [makeRun('a', 'completed')], next_cursor: null });
  await vi.advanceTimersByTimeAsync(500);
  expect(live.textContent?.trim()).toBe('Run b failed. Run a completed.');
  observe.mockClear();
  emit('run_failed', { run: makeRun('b', 'failed') });
  emit('run_completed', { run: makeRun('a', 'completed') });
  resync.set({ epoch: 2, reason: { reason: 'lagged', skipped: 1 } });
  await vi.advanceTimersByTimeAsync(1_000);
  expect(observe).not.toHaveBeenCalled();
  observer.disconnect();
});
