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
import { ApiNetworkError, ApiProblemError } from '../../lib/api/client';
import type { EventEnvelope, ResyncSignal } from '../../lib/api/events';
import { mockNativeDialog } from '../../test/dialog';
import type { Run } from '../jobs/run-model';
import RunDetail from './RunDetail.svelte';

const fake = vi.hoisted(() => ({
  client: {
    getRecordedRun: vi.fn(),
    getResolvedWorkflow: vi.fn(),
    getRun: vi.fn(),
    getRunManifestText: vi.fn(),
    listRunEvents: vi.fn(),
    listOutputs: vi.fn(),
    listRuns: vi.fn(),
    rerunRun: vi.fn(),
  },
  listeners: new Map<string, (event: EventEnvelope) => void>(),
  navigate: vi.fn(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake.client,
}));
vi.mock('../../navigation', () => ({ navigateHash: fake.navigate }));
vi.mock('../../lib/api/events', () => ({
  events: {
    status: writable('open'),
    resync: writable<ResyncSignal | null>(null),
    on: (type: string, listener: (event: EventEnvelope) => void) => {
      fake.listeners.set(type, listener);
      return () => fake.listeners.delete(type);
    },
  },
}));
import { events } from '../../lib/api/events';

const run: Run = {
  id: 'one',
  prompt_id: 'prompt',
  workflow_id: 'landscape',
  workflow_version: 'v1',
  workflow_sha256: 'graphhash',
  status: 'completed',
  parameters: { seed: 18446744073709551615n, prompt: 'Coast' },
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  rerun_of: 'parent',
};
const output = {
  id: 'one:1',
  run_id: 'one',
  workflow_id: 'landscape',
  node_id: '2',
  kind: 'image',
  source_filename: 'result.png',
  content_type: 'image/png',
  sha256: 'outputhash',
  byte_size: 10,
  created_at: run.created_at,
  tags: [],
};
const manifest = JSON.stringify({
  client_id: 'client',
  prompt_id: 'prompt',
  comfyui_base_url: 'http://127.0.0.1:8188',
  preset: 'sunset',
  workflow: {
    id: 'landscape',
    version: 'v1',
    sha256: 'graphhash',
    manifest_sha256: 'manifesthash',
  },
  parameters: { seed: 'SEED_SENTINEL', prompt: 'Coast' },
  source_inputs: [{ parameter: 'image', sha256: 'inputhash' }],
  comfyui_history: {
    prompt: { prompt: [1, 'prompt', { 7: { class_type: 'Sampler' } }] },
  },
}).replace('"SEED_SENTINEL"', '18446744073709551615');
const event = {
  id: 1,
  run_id: 'one',
  event_type: 'created',
  message: 'Created event',
  payload: null,
  created_at: run.created_at,
};
mockNativeDialog();
beforeEach(() => {
  vi.resetAllMocks();
  fake.listeners.clear();
  sessionStorage.clear();
  fake.client.getRecordedRun.mockResolvedValue(run);
  fake.client.getRun.mockResolvedValue(run);
  fake.client.getResolvedWorkflow.mockResolvedValue({
    '7': { class_type: 'Sampler' },
  });
  fake.client.getRunManifestText.mockResolvedValue({
    text: manifest,
    integrityVerified: true,
  });
  fake.client.listRunEvents.mockResolvedValue({ events: [event] });
  fake.client.listOutputs.mockResolvedValue({ outputs: [output] });
  fake.client.listRuns.mockResolvedValue({
    runs: [{ ...run, id: 'child', rerun_of: 'one' }],
  });
  fake.client.rerunRun.mockResolvedValue({
    ...run,
    id: 'new',
    rerun_of: 'one',
  });
});
afterEach(() => {
  cleanup();
  sessionStorage.clear();
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set(null);
});
async function start() {
  render(RunDetail, { id: 'one' });
  await screen.findByText('manifesthash');
}

test('renders recorded provenance, outputs, relationships and lazy workflow JSON', async () => {
  await start();
  for (const value of [
    'landscape',
    'v1',
    'graphhash',
    'manifesthash',
    'sunset',
    'prompt',
    'client',
    'http://127.0.0.1:8188',
    '18446744073709551615',
    'Coast',
    'inputhash',
    'outputhash',
    'Created event',
  ])
    expect(screen.getAllByText(value, { exact: false }).length).toBeGreaterThan(
      0,
    );
  expect(
    screen.getByRole('link', { name: 'parent' }).getAttribute('href'),
  ).toBe('#/runs/parent');
  expect(screen.getByRole('link', { name: 'child' }).getAttribute('href')).toBe(
    '#/runs/child',
  );
  expect(
    screen.getByRole('link', { name: 'result.png' }).getAttribute('href'),
  ).toBe('#/gallery?output=one%3A1');
  expect(screen.queryByText(/Sampler/)).toBeNull();
  const details = screen.getByText('Show resolved workflow JSON')
    .parentElement as HTMLDetailsElement;
  expect(fake.client.getResolvedWorkflow).not.toHaveBeenCalled();
  details.open = true;
  fireEvent(details, new Event('toggle'));
  expect(await screen.findByText(/Sampler/)).toBeTruthy();
  expect(fake.client.getResolvedWorkflow).not.toHaveBeenCalled();
});

test('refreshes live status and timeline on events and resync', async () => {
  await start();
  fake.client.getRecordedRun.mockResolvedValue({ ...run, status: 'failed' });
  fake.client.listRunEvents.mockResolvedValue({
    events: [
      { ...event, id: 2, event_type: 'failed', message: 'Backend failed' },
    ],
  });
  fake.listeners.get('run_failed')?.({
    type: 'run_failed',
    run: {
      ...run,
      status: 'failed',
      client_id: 'client',
      generation_mode: 'generic',
      output_dir: 'unused',
    },
    id: 2,
    bus_id: 'bus',
    timestamp: run.updated_at,
  } as EventEnvelope);
  expect(
    await screen.findByText('Backend failed', { exact: false }),
  ).toBeTruthy();
  expect(
    screen.getAllByText('Failed', { exact: false }).length,
  ).toBeGreaterThan(0);
  const count = fake.client.getRecordedRun.mock.calls.length;
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 1,
    reason: { reason: 'cursor_unavailable' },
  });
  await waitFor(() =>
    expect(fake.client.getRecordedRun.mock.calls.length).toBeGreaterThan(count),
  );
});

test('confirms a new run without mutating its source', async () => {
  await start();
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Re-run' }));
  expect(
    within(screen.getByRole('dialog')).getByText(
      /original run will remain unchanged/i,
    ),
  ).toBeTruthy();
  expect(fake.client.rerunRun).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
  await waitFor(() =>
    expect(fake.navigate).toHaveBeenCalledWith('#/runs/new?created=rerun'),
  );
  expect(fake.client.rerunRun).toHaveBeenCalledWith(
    'one',
    expect.stringMatching(/^[0-9a-f-]{36}$/),
  );
});

test('offers Remix after a provenance mismatch', async () => {
  fake.client.rerunRun.mockRejectedValue(
    new ApiProblemError({
      type: 'urn:genaimage:problem:rerun-mismatch',
      status: 409,
      title: 'Mismatch',
      detail: 'Changed',
      recovery: 'Restore the original files.',
    }),
  );
  await start();
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Re-run' }));
  await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
  expect(await screen.findByText('Restore the original files.')).toBeTruthy();
  expect(
    screen.getByRole('link', { name: 'Remix' }).getAttribute('href'),
  ).toContain('#/generate?');
});

test('retains the same idempotency key after an uncertain result', async () => {
  fake.client.rerunRun.mockRejectedValueOnce(
    new ApiNetworkError('Check Jobs.', new Error('offline')),
  );
  await start();
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Re-run' }));
  await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
  await user.click(
    await screen.findByRole('button', { name: 'Retry same request' }),
  );
  await waitFor(() => expect(fake.client.rerunRun).toHaveBeenCalledTimes(2));
  expect(fake.client.rerunRun.mock.calls[0][1]).toBe(
    fake.client.rerunRun.mock.calls[1][1],
  );
});

test('offers a workflow-only Remix link until Generate is merged', async () => {
  await start();
  const href = screen
    .getByRole('link', { name: 'Remix in Generate' })
    .getAttribute('href');
  expect(href).toBe('#/generate?workflow=landscape');
  expect(
    screen.getByText(/Recorded parameters are not yet carried across/),
  ).toBeTruthy();
});

test('downloads exact manifest bytes', async () => {
  const create = vi.fn(() => 'blob:manifest');
  const revoke = vi.fn();
  vi.stubGlobal(
    'URL',
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke }),
  );
  const click = vi
    .spyOn(HTMLAnchorElement.prototype, 'click')
    .mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.download).toBe('run-one.manifest.json');
    });
  await start();
  await userEvent.click(
    screen.getByRole('button', { name: 'Download manifest' }),
  );
  expect(create).toHaveBeenCalled();
  await waitFor(() => expect(revoke).toHaveBeenCalledWith('blob:manifest'));
  expect(click).toHaveBeenCalled();
  click.mockRestore();
  vi.unstubAllGlobals();
});

test('gates uncertain source runs and reports not-found errors', async () => {
  fake.client.getRecordedRun.mockResolvedValueOnce({
    ...run,
    status: 'unknown_submission',
  });
  await start();
  expect(screen.queryByRole('button', { name: 'Re-run' })).toBeNull();
  expect(screen.getByRole('link', { name: 'Reconcile in Jobs' })).toBeTruthy();
  cleanup();
  fake.client.getRecordedRun.mockRejectedValue(
    new ApiProblemError({
      status: 404,
      type: 'about:blank',
      title: 'Not found',
      detail: 'No such run',
      recovery: 'Check the ID.',
    }),
  );
  render(RunDetail, { id: 'missing' });
  expect(await screen.findByText('No such run')).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Reload run' })).toBeTruthy();
});

test('loads additional output pages without duplicating earlier outputs', async () => {
  fake.client.listOutputs.mockResolvedValueOnce({
    outputs: [output],
    next_cursor: 'next',
  });
  await start();
  fake.client.listOutputs.mockResolvedValueOnce({
    outputs: [output, { ...output, id: 'one:2', source_filename: 'more.png' }],
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Load more outputs' }),
  );
  expect(await screen.findByRole('link', { name: 'more.png' })).toBeTruthy();
  expect(screen.getAllByRole('link', { name: 'result.png' })).toHaveLength(1);
  expect(fake.client.listOutputs).toHaveBeenLastCalledWith(
    expect.objectContaining({ run: 'one', cursor: 'next' }),
    expect.anything(),
  );
});

test('preserves an uncertain key if a later status check fails', async () => {
  sessionStorage.setItem('genaimage.rerun.one', 'previous-key');
  fake.client.getRun.mockRejectedValue(
    new ApiProblemError({
      status: 404,
      type: 'about:blank',
      title: 'Not found',
      detail: 'No such run',
      recovery: 'Check the ID.',
    }),
  );
  await start();
  await userEvent.click(
    screen.getByRole('button', { name: 'Retry same request' }),
  );
  expect(await screen.findByText('No such run')).toBeTruthy();
  expect(sessionStorage.getItem('genaimage.rerun.one')).toBe('previous-key');
  expect(fake.client.rerunRun).not.toHaveBeenCalled();
  expect(
    screen.getByRole('button', { name: 'Retry same request' }),
  ).toBeTruthy();
});

test('does not re-run if the source became uncertain during confirmation', async () => {
  fake.client.getRun.mockResolvedValue({
    ...run,
    status: 'unknown_submission',
  });
  await start();
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Re-run' }));
  await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
  expect(
    await screen.findByText(/source submission is uncertain/i),
  ).toBeTruthy();
  expect(fake.client.rerunRun).not.toHaveBeenCalled();
});

test('does not permit an unreviewable re-run when the manifest is unavailable', async () => {
  fake.client.getRunManifestText.mockRejectedValue(new Error('not available'));
  render(RunDetail, { id: 'one' });
  expect(
    await screen.findByText(/Re-run requires a verified manifest/),
  ).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Re-run' })).toBeNull();
  expect(
    screen.queryByRole('button', { name: 'Download manifest' }),
  ).toBeNull();
});

test('prefers manifest parameters and workflow provenance over the run snapshot', async () => {
  fake.client.getRunManifestText.mockResolvedValue({
    integrityVerified: true,
    text: JSON.stringify({
      preset: null,
      source_inputs: [],
      parameters: { seed: 42, prompt: 'From manifest' },
      workflow: {
        id: 'manifest-workflow',
        version: 'v2',
        sha256: 'manifest-graph',
      },
    }),
  });
  render(RunDetail, { id: 'one' });
  expect(await screen.findByText('From manifest')).toBeTruthy();
  expect(screen.getByText('manifest-workflow')).toBeTruthy();
  expect(screen.getByText('manifest-graph')).toBeTruthy();
  expect(screen.queryByText('Coast')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Re-run' })).toBeNull();
});

test('fetches the archived workflow lazily when history has no graph', async () => {
  fake.client.getRunManifestText.mockResolvedValue({
    integrityVerified: true,
    text: JSON.stringify({ preset: null, source_inputs: [] }),
  });
  render(RunDetail, { id: 'one' });
  const details = (await screen.findByText('Show resolved workflow JSON'))
    .parentElement as HTMLDetailsElement;
  expect(fake.client.getResolvedWorkflow).not.toHaveBeenCalled();
  details.open = true;
  fireEvent(details, new Event('toggle'));
  expect(await screen.findByText(/Sampler/)).toBeTruthy();
  expect(fake.client.getResolvedWorkflow).toHaveBeenCalledTimes(1);
});

test('does not present a completed run as confirmed without outputs', async () => {
  fake.client.listOutputs.mockResolvedValue({ outputs: [] });
  await start();
  expect(screen.getByText('Completed · outputs not confirmed')).toBeTruthy();
});

test('does not allow re-run if recorded numbers cannot be displayed exactly', async () => {
  fake.client.getRecordedRun.mockResolvedValue({
    ...run,
    parameters: { seed: Number.MAX_SAFE_INTEGER + 2 },
  });
  fake.client.getRunManifestText.mockResolvedValue({
    text: JSON.stringify({
      preset: null,
      source_inputs: [],
      workflow: { manifest_sha256: 'manifesthash' },
    }),
    integrityVerified: true,
  });
  await start();
  expect(
    screen.getByText('Cannot display exactly; see the run record'),
  ).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Re-run' })).toBeNull();
});

test('marks unverified manifests and prevents misleading re-run confirmation', async () => {
  fake.client.getRunManifestText.mockResolvedValue({
    text: manifest,
    integrityVerified: false,
  });
  await start();
  expect(
    screen.getByText('Manifest integrity could not be verified.'),
  ).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Re-run' })).toBeNull();
});
