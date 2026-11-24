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
  BindingFloat,
} from '../../lib/api/client';
import { restoreFormState } from '../generate/share';
import type { Workflow } from '../generate/model';
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
  subscriptions: 0,
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake.client,
}));
vi.mock('../../navigation', () => ({ navigateHash: fake.navigate }));
vi.mock('../../lib/api/events', () => {
  function tracked<T>(value: T) {
    const store = writable(value);
    return {
      ...store,
      subscribe: (listener: (value: T) => void) => {
        fake.subscriptions++;
        const unsubscribe = store.subscribe(listener);
        return () => {
          fake.subscriptions--;
          unsubscribe();
        };
      },
    };
  }
  return {
    events: {
      status: tracked('open'),
      resync: tracked<ResyncSignal | null>(null),
      on: (type: string, listener: (event: EventEnvelope) => void) => {
        fake.listeners.set(type, listener);
        return () => fake.listeners.delete(type);
      },
    },
  };
});
import { events } from '../../lib/api/events';

const run: Run = {
  id: 'one',
  prompt_id: 'prompt',
  workflow_id: 'landscape',
  workflow_version: 'v1',
  workflow_sha256: 'graphhash',
  workflow_revision: 'source-revision',
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
    graph: { '7': { class_type: 'Sampler' } },
    integrity: 'verified',
    etag: '"abc"',
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
  expect(
    screen.getByText(/ComfyUI history graph.*verified manifest/),
  ).toBeTruthy();
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
  const primary = screen
    .getByRole('link', { name: 'Remix in Generate' })
    .getAttribute('href')!;
  expect(screen.getByRole('link', { name: 'Remix' }).getAttribute('href')).toBe(
    primary,
  );
  expect(new URLSearchParams(primary.split('?')[1]).get('wr')).toBe(
    'source-revision',
  );
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

test('remixes exact recorded bindings through the Generate decoder', async () => {
  fake.client.getRecordedRun.mockImplementation((_id, options) =>
    Promise.resolve(
      options?.exactBindings
        ? {
            ...run,
            parameters: {
              seed: 18446744073709551615n,
              prompt: 'Coast  at\nnight',
              scale: new BindingFloat(1),
            },
          }
        : run,
    ),
  );
  await start();
  const href = screen
    .getByRole('link', { name: 'Remix in Generate' })
    .getAttribute('href')!;
  const query = new URLSearchParams(href.split('?')[1]);
  const workflow: Workflow = {
    id: 'landscape',
    version: 'v1',
    revision: 'revision',
    schema_version: 2,
    title: 'Landscape',
    workflow: 'graph.json',
    outputs: [],
    presets: {},
    parameters: {
      seed: {
        node: '1',
        input: 'seed',
        kind: 'seed',
        required: true,
        description: '',
      },
      prompt: {
        node: '1',
        input: 'prompt',
        kind: 'string',
        required: true,
        description: '',
      },
      scale: {
        node: '1',
        input: 'scale',
        kind: 'number',
        required: true,
        description: '',
      },
    },
  };
  const restored = restoreFormState(workflow, query);
  expect(query.get('workflow')).toBe('landscape');
  expect(query.get('wr')).toBe('source-revision');
  expect(query.get('unverified_source')).toBeNull();
  expect(restored.revisionNotice).toMatch(/revision differs/);
  expect(restored.warnings).toEqual([]);
  expect(restored.fields.get('seed')?.value).toBe(18446744073709551615n);
  expect(restored.fields.get('seed')?.seedMode).toBe('locked');
  expect(restored.fields.get('prompt')?.value).toBe('Coast  at\nnight');
  expect(restored.fields.get('scale')?.value).toEqual(new BindingFloat(1));
  expect(fake.client.getRecordedRun).toHaveBeenCalledWith(
    'one',
    expect.objectContaining({ exactBindings: true }),
  );
  expect(fake.client.rerunRun).not.toHaveBeenCalled();
  expect(fake.navigate).not.toHaveBeenCalled();
});

test('warns on both Remix links when the source revision is unknown', async () => {
  fake.client.getRecordedRun.mockResolvedValue({
    ...run,
    workflow_revision: undefined,
  });
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
  const link = screen
    .getByRole('link', { name: 'Remix in Generate' })
    .getAttribute('href')!;
  const query = new URLSearchParams(link.split('?')[1]);
  expect(query.get('wr')).toBeNull();
  expect(query.get('unverified_source')).toBe('1');
  const warning =
    'Remix uses the current workflow; it cannot be verified against the source run.';
  expect(screen.getByText(warning)).toBeTruthy();
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Re-run' }));
  await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
  await screen.findByText('Restore the original files.');
  expect(screen.getByRole('link', { name: 'Remix' }).getAttribute('href')).toBe(
    link,
  );
  expect(screen.getAllByText(warning)).toHaveLength(2);
});

test('does not offer Remix when exact binding reads fail', async () => {
  fake.client.getRecordedRun.mockImplementation((_id, options) =>
    options?.exactBindings
      ? Promise.reject(new Error('Cannot read exactly'))
      : Promise.resolve(run),
  );
  await start();
  expect(screen.queryByRole('link', { name: /Remix/i })).toBeNull();
  expect(screen.getByText(/Remix is unavailable/)).toBeTruthy();
});

test('does not silently Remix a run without a recorded parameter object', async () => {
  fake.client.getRecordedRun.mockImplementation((_id, options) =>
    Promise.resolve(
      options?.exactBindings ? { ...run, parameters: null } : run,
    ),
  );
  await start();
  expect(screen.queryByRole('link', { name: /Remix/i })).toBeNull();
  expect(screen.getByText(/Remix is unavailable/)).toBeTruthy();
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
  expect(
    screen.getByText(/Verified archive.*recorded before submission/),
  ).toBeTruthy();
  expect(screen.getByText('Archive ETag: "abc"')).toBeTruthy();
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

test('marks legacy archives and clears stale graphs when the next fetch fails', async () => {
  fake.client.getRunManifestText.mockResolvedValue({
    integrityVerified: true,
    text: JSON.stringify({ preset: null, source_inputs: [] }),
  });
  fake.client.getResolvedWorkflow
    .mockResolvedValueOnce({
      graph: { old: 'Old graph' },
      integrity: 'unverified',
      etag: '"legacy"',
    })
    .mockRejectedValue(new Error('archive changed'));
  render(RunDetail, { id: 'one' });
  const details = (await screen.findByText('Show resolved workflow JSON'))
    .parentElement as HTMLDetailsElement;
  details.open = true;
  fireEvent(details, new Event('toggle'));
  expect(await screen.findByText(/Old graph/)).toBeTruthy();
  expect(
    screen.getByText('Unverified archive — cannot confirm this is what ran'),
  ).toBeTruthy();
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 9,
    reason: { reason: 'cursor_unavailable' },
  });
  expect(await screen.findByText('archive changed')).toBeTruthy();
  expect(screen.queryByText(/Old graph/)).toBeNull();
  expect(
    screen.queryByText('Unverified archive — cannot confirm this is what ran'),
  ).toBeNull();
});

test.each([
  { id: 'one', rerun_of: 'one' },
  { id: 'new', rerun_of: 'other' },
  { id: '', rerun_of: 'one' },
])(
  'keeps the same key when a successful response does not identify a re-run: %j',
  async (invalid) => {
    fake.client.rerunRun.mockResolvedValueOnce(invalid);
    await start();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Re-run' }));
    await user.click(screen.getByRole('button', { name: 'Confirm re-run' }));
    const retry = await screen.findByRole('button', {
      name: 'Retry same request',
    });
    expect(fake.navigate).not.toHaveBeenCalled();
    const key = fake.client.rerunRun.mock.calls[0][1];
    expect(sessionStorage.getItem('genaimage.rerun.one')).toBe(key);
    await user.click(retry);
    await waitFor(() => expect(fake.client.rerunRun).toHaveBeenCalledTimes(2));
    expect(fake.client.rerunRun.mock.calls[1][1]).toBe(key);
    expect(fake.navigate).toHaveBeenCalledWith('#/runs/new?created=rerun');
  },
);

test('replaces the bounded latest event window after a large resync', async () => {
  const makeEvents = (first: number) =>
    Array.from({ length: 200 }, (_, index) => ({
      ...event,
      id: first + index,
      message: `Event ${first + index}`,
    }));
  fake.client.listRunEvents.mockResolvedValue({ events: makeEvents(51) });
  await start();
  expect(
    screen.getByText('Showing the latest 200 recorded events.'),
  ).toBeTruthy();
  expect(screen.getByText(/Event 51$/)).toBeTruthy();
  expect(screen.getByText(/Event 250$/)).toBeTruthy();
  expect(fake.client.listRunEvents).toHaveBeenCalledWith(
    'one',
    { limit: 200 },
    expect.anything(),
  );
  fake.client.listRunEvents.mockResolvedValue({ events: makeEvents(501) });
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 10,
    reason: { reason: 'cursor_unavailable' },
  });
  expect(await screen.findByText(/Event 700$/)).toBeTruthy();
  expect(screen.queryByText(/Event 250$/)).toBeNull();
  expect(screen.getByText(/Event 501$/)).toBeTruthy();
  expect(
    within(document.querySelector('ol')!).getAllByRole('listitem'),
  ).toHaveLength(200);
});

test('preserves prompt whitespace on the page and in the confirmation', async () => {
  const prompt = 'First  line\nSecond   line';
  fake.client.getRecordedRun.mockResolvedValue({
    ...run,
    parameters: { seed: 18446744073709551615n, prompt },
  });
  fake.client.getRunManifestText.mockResolvedValue({
    text: manifest.replace('"Coast"', JSON.stringify(prompt)),
    integrityVerified: true,
  });
  await start();
  const value = [...document.querySelectorAll('.recorded-value')].find(
    (item) => item.textContent === prompt,
  ) as HTMLElement;
  expect(value).toBeTruthy();
  expect(getComputedStyle(value).whiteSpace).toBe('pre-wrap');
  await userEvent.click(screen.getByRole('button', { name: 'Re-run' }));
  const dialog = screen.getByRole('dialog');
  const shown = [...dialog.querySelectorAll('.recorded-value')].find(
    (item) => item.textContent === prompt,
  ) as HTMLElement;
  expect(shown).toBeTruthy();
  expect(getComputedStyle(shown).whiteSpace).toBe('pre-wrap');
});

test('announces status changes politely and unsubscribes on unmount', async () => {
  await start();
  const region = document.querySelector('[aria-live="polite"]')!;
  expect(region.textContent).toContain('Outputs confirmed for this run.');
  fake.client.getRecordedRun.mockResolvedValue({ ...run, status: 'failed' });
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 11,
    reason: { reason: 'cursor_unavailable' },
  });
  await waitFor(() =>
    expect(region.textContent).toContain('Run status changed to Failed.'),
  );
  expect(fake.listeners.size).toBeGreaterThan(0);
  expect(fake.subscriptions).toBeGreaterThan(0);
  cleanup();
  expect(fake.listeners.size).toBe(0);
  expect(fake.subscriptions).toBe(0);
});

test('clears the history graph while switching to the archive source', async () => {
  await start();
  const details = screen.getByText('Show resolved workflow JSON')
    .parentElement as HTMLDetailsElement;
  details.open = true;
  fireEvent(details, new Event('toggle'));
  expect(await screen.findByText(/Sampler/)).toBeTruthy();
  let finish!: (value: unknown) => void;
  fake.client.getResolvedWorkflow.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  fake.client.getRunManifestText.mockResolvedValue({
    integrityVerified: true,
    text: JSON.stringify({ preset: null, source_inputs: [] }),
  });
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 12,
    reason: { reason: 'cursor_unavailable' },
  });
  expect(await screen.findByText('Loading workflow…')).toBeTruthy();
  expect(screen.queryByText(/Sampler/)).toBeNull();
  finish({
    graph: { replacement: 'New graph' },
    integrity: 'verified',
    etag: '"abc"',
  });
  expect(await screen.findByText(/New graph/)).toBeTruthy();
  expect(
    screen.getByText(/Verified archive.*recorded before submission/),
  ).toBeTruthy();
});

test('announces when delayed outputs are confirmed', async () => {
  let finish!: (page: { outputs: (typeof output)[] }) => void;
  fake.client.listOutputs.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  render(RunDetail, { id: 'one' });
  expect(
    await screen.findByText('Completed · outputs not confirmed'),
  ).toBeTruthy();
  const region = document.querySelector('[aria-live="polite"]')!;
  expect(region.textContent).not.toContain('Outputs confirmed');
  finish({ outputs: [output] });
  await waitFor(() =>
    expect(region.textContent).toContain('Outputs confirmed for this run.'),
  );
  expect(screen.getByRole('link', { name: 'result.png' })).toBeTruthy();
});
