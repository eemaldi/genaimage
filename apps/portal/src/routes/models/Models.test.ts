import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, type api } from '../../lib/api/client';
import { mockNativeDialog } from '../../test/dialog';
import Models from './Models.svelte';
import { formatBytes, validateModelName, type ModelPull } from './model';

const fake = vi.hoisted(() => ({
  llmModels: vi.fn<typeof api.llmModels>(),
  checkpoints: vi.fn<typeof api.checkpoints>(),
  listModelPulls: vi.fn<typeof api.listModelPulls>(),
  getModelPull: vi.fn<typeof api.getModelPull>(),
  startModelPull: vi.fn<typeof api.startModelPull>(),
  cancelModelPull: vi.fn<typeof api.cancelModelPull>(),
  streamModelPull: vi.fn<typeof api.streamModelPull>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

mockNativeDialog();
const props = { params: {}, query: new URLSearchParams() };

function pull(overrides: Partial<ModelPull> = {}): ModelPull {
  return {
    id: 'p1',
    name: 'llama3.2:3b',
    state: 'running',
    status: 'pulling manifest',
    layer: null,
    total_bytes: null,
    completed_bytes: null,
    error: null,
    started_at: '2026-01-01T00:00:00Z',
    finished_at: null,
    sequence: 1,
    ...overrides,
  };
}

function problem(status: number, type: string) {
  return new ApiProblemError({
    type,
    title: `Problem ${status}`,
    status,
    detail: `Detail ${status}`,
    recovery: `Recovery ${status}`,
  });
}

function llm(over: Record<string, unknown> = {}) {
  return {
    source: 'ollama' as const,
    pull_supported: true,
    pull_unsupported_reason: null,
    models: [
      {
        name: 'qwen3:8b',
        size_bytes: 5_200_000_000,
        digest: 'abc',
        modified_at: '2026-02-03T04:05:06Z',
      },
    ],
    roles: [
      { role: 'text' as const, model: 'qwen3:8b', state: 'present' as const },
      { role: 'vision' as const, model: 'llava', state: 'missing' as const },
    ],
    ...over,
  };
}

/** A stream that yields the frames, then ends (a cut unless terminal). */
function stream(...pulls: ModelPull[]) {
  return (async function* () {
    for (const data of pulls) yield { event: 'pull_progress', data };
  })();
}

/** A stream that stays open until aborted. */
function hanging(first?: ModelPull) {
  return (async function* () {
    if (first) yield { event: 'pull_progress', data: first };
    await new Promise(() => undefined);
  })();
}

beforeEach(() => {
  fake.llmModels.mockResolvedValue(llm());
  fake.checkpoints.mockResolvedValue({
    checkpoints: ['sdxl.safetensors'],
    locations: [
      {
        environment: 'compose',
        directory: '/opt/comfyui/models/checkpoints',
        steps: ['Copy a file in with: docker compose cp <file> comfyui:/x/'],
      },
    ],
    refresh_note: 'Refresh ComfyUI.',
  });
  fake.listModelPulls.mockResolvedValue({ pulls: [] });
});
afterEach(() => {
  cleanup();
  for (const mock of Object.values(fake)) mock.mockReset();
});

async function confirmPull(name = 'llama3.2:3b') {
  const user = userEvent.setup();
  const input = await screen.findByLabelText<HTMLInputElement>('Model name');
  await waitFor(() => expect(input.disabled).toBe(false));
  await user.type(input, name);
  await user.click(screen.getByRole('button', { name: 'Pull…' }));
  return user;
}

test('renders roles, installed models and checkpoints', async () => {
  render(Models, props);
  await screen.findByText('qwen3:8b', { selector: 'code' });
  expect(document.querySelector('[data-role="text"]')?.textContent).toContain(
    'Present',
  );
  expect(document.querySelector('[data-role="vision"]')?.textContent).toContain(
    'Missing',
  );
  expect(screen.getByText('5.2 GB')).toBeTruthy();
  expect(screen.getByText('2026-02-03T04:05:06Z')).toBeTruthy();
  expect(await screen.findByText('sdxl.safetensors')).toBeTruthy();
  expect(screen.getByText('Refresh ComfyUI.')).toBeTruthy();
  expect(screen.getByText('docker compose cp <file> comfyui:/x/').tagName).toBe(
    'CODE',
  );
});

test('shows the unsupported reason verbatim and disables the form', async () => {
  fake.llmModels.mockResolvedValue(
    llm({
      source: 'openai',
      pull_supported: false,
      pull_unsupported_reason: 'This server has no pull API.',
    }),
  );
  render(Models, props);
  expect(await screen.findByText(/This server has no pull API\./)).toBeTruthy();
  expect(
    (screen.getByLabelText('Model name') as HTMLInputElement).disabled,
  ).toBe(true);
  expect(
    (screen.getByRole('button', { name: 'Pull…' }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
});

test('a pull needs explicit confirmation and posts exactly once', async () => {
  fake.startModelPull.mockResolvedValue({
    pull: pull(),
    existingId: undefined,
  });
  fake.streamModelPull.mockImplementation(async () => hanging());
  render(Models, props);
  const user = await confirmPull();
  const dialog = await screen.findByRole('dialog');
  expect(dialog.textContent).toContain('llama3.2:3b');
  expect(dialog.textContent).toContain('configured Ollama');
  expect(fake.startModelPull).not.toHaveBeenCalled();
  await user.click(
    screen.getByRole('button', { name: /Download llama3.2:3b/ }),
  );
  await waitFor(() => expect(fake.startModelPull).toHaveBeenCalledTimes(1));
  expect(fake.startModelPull.mock.calls[0][0]).toBe('llama3.2:3b');
  await screen.findByText(/pulling manifest/);
});

test('declining the confirmation downloads nothing', async () => {
  render(Models, props);
  const user = await confirmPull();
  await user.click(
    await screen.findByRole('button', { name: 'Do not download' }),
  );
  expect(fake.startModelPull).not.toHaveBeenCalled();
});

test('an obviously invalid name is rejected before any request', async () => {
  render(Models, props);
  const user = userEvent.setup();
  const input = await screen.findByLabelText<HTMLInputElement>('Model name');
  await waitFor(() => expect(input.disabled).toBe(false));
  await user.type(input, 'a b');
  await user.click(screen.getByRole('button', { name: 'Pull…' }));
  expect(await screen.findByText(/cannot contain spaces/)).toBeTruthy();
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(validateModelName('http://x/y')).toMatch(/URL/);
  expect(validateModelName('llama3.2:3b')).toBeUndefined();
});

test('409 attaches to the existing pull without a second POST', async () => {
  fake.startModelPull.mockResolvedValue({ pull: undefined, existingId: 'p9' });
  fake.getModelPull.mockResolvedValue(
    pull({ id: 'p9', status: 'downloading', sequence: 4 }),
  );
  fake.streamModelPull.mockImplementation(async () => hanging());
  render(Models, props);
  const user = await confirmPull();
  await user.click(await screen.findByRole('button', { name: /Download/ }));
  await screen.findByText(/already being pulled/);
  await screen.findByText(/downloading/);
  expect(fake.startModelPull).toHaveBeenCalledTimes(1);
  expect(fake.streamModelPull.mock.calls[0][0]).toBe('p9');
});

test('a lower sequence frame after a higher one is ignored', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () =>
    hanging(pull({ sequence: 1 })),
  );
  // Newer frame first, then an older one arrives late.
  fake.streamModelPull.mockImplementationOnce(async () =>
    (async function* () {
      yield {
        event: 'pull_progress',
        data: pull({
          sequence: 9,
          status: 'newer',
          total_bytes: 100,
          completed_bytes: 50,
        }),
      };
      yield {
        event: 'pull_progress',
        data: pull({ sequence: 3, status: 'older' }),
      };
      await new Promise(() => undefined);
    })(),
  );
  render(Models, props);
  await screen.findByText(/newer/);
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(screen.queryByText(/older/)).toBeNull();
  expect(screen.getByText(/\(50%\)/)).toBeTruthy();
});

test('a cut stream re-reads the pull and is never reported as success', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull
    .mockImplementationOnce(async () => stream(pull({ sequence: 2 })))
    .mockImplementation(async () => hanging());
  fake.getModelPull.mockResolvedValue(pull({ sequence: 3, status: 'resumed' }));
  render(Models, props);
  await screen.findByText(/resumed/);
  expect(fake.getModelPull).toHaveBeenCalledWith('p1', expect.anything());
  await waitFor(() => expect(fake.streamModelPull).toHaveBeenCalledTimes(2));
  expect(screen.queryByText('Installed')).toBeNull();
  expect(fake.llmModels).toHaveBeenCalledTimes(1);
});

test('404 on a known pull shows unknown and refreshes the models', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () => stream());
  fake.getModelPull.mockRejectedValue(problem(404, 'urn:x:pull-not-found'));
  render(Models, props);
  await screen.findByText(/the server may have restarted/);
  expect(screen.queryByText('Installed')).toBeNull();
  await waitFor(() => expect(fake.llmModels).toHaveBeenCalledTimes(2));
});

test('cancel stops the pull and warns it may still have installed', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () => hanging());
  fake.cancelModelPull.mockResolvedValue(
    pull({ state: 'cancelled', status: 'cancelled', sequence: 2 }),
  );
  render(Models, props);
  const user = userEvent.setup();
  expect(await screen.findByText(/may still have installed/)).toBeTruthy();
  await user.click(
    await screen.findByRole('button', { name: /Cancel pull of llama3.2:3b/ }),
  );
  await screen.findByText('Cancelled');
  expect(fake.cancelModelPull).toHaveBeenCalledTimes(1);
  await waitFor(() => expect(fake.llmModels).toHaveBeenCalledTimes(2));
  expect(screen.queryByRole('button', { name: /Cancel pull/ })).toBeNull();
});

test('a succeeded pull refreshes the installed models', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () =>
    (async function* () {
      yield {
        event: 'pull_succeeded',
        data: pull({ state: 'succeeded', status: 'success', sequence: 5 }),
      };
    })(),
  );
  render(Models, props);
  await screen.findByText(/was pulled and is installed/);
  await screen.findByText('Installed');
  expect(fake.llmModels).toHaveBeenCalledTimes(2);
});

test('a failed pull shows its error and does not refresh as success', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () =>
    stream(pull({ state: 'failed', error: 'disk full', sequence: 3 })),
  );
  render(Models, props);
  await screen.findByText('disk full');
  expect(screen.queryByText('Installed')).toBeNull();
  expect(fake.llmModels).toHaveBeenCalledTimes(1);
});

test('checkpoints 503 is an error, not an empty list', async () => {
  fake.checkpoints.mockRejectedValue(
    problem(503, 'urn:genaimage:problem:comfyui-unavailable'),
  );
  render(Models, props);
  await screen.findByText('Recovery 503');
  expect(screen.queryByText(/reports no checkpoints/)).toBeNull();
  expect(screen.queryByText(/Installed checkpoints/)).toBeNull();
});

test('reloading with a running pull re-attaches and shows progress', async () => {
  fake.listModelPulls.mockResolvedValue({
    pulls: [
      pull({
        total_bytes: 2_000_000_000,
        completed_bytes: 500_000_000,
        layer: 'sha256:abcd',
      }),
    ],
  });
  fake.streamModelPull.mockImplementation(async () => hanging());
  render(Models, props);
  await screen.findByText(/500 MB of 2\.0 GB \(25%\)/);
  expect(screen.getByText(/layer sha256:abcd/)).toBeTruthy();
  await waitFor(() =>
    expect(fake.streamModelPull).toHaveBeenCalledWith('p1', expect.anything()),
  );
});

test('unmounting aborts the stream', async () => {
  fake.listModelPulls.mockResolvedValue({ pulls: [pull()] });
  fake.streamModelPull.mockImplementation(async () => hanging());
  const view = render(Models, props);
  await waitFor(() => expect(fake.streamModelPull).toHaveBeenCalled());
  const signal = fake.streamModelPull.mock.calls[0][1]?.signal;
  expect(signal?.aborted).toBe(false);
  view.unmount();
  expect(signal?.aborted).toBe(true);
});

test('formats bytes for display only', () => {
  expect(formatBytes(999)).toBe('999 B');
  expect(formatBytes(1_500)).toBe('1.5 KB');
  expect(formatBytes(9_007_199_254_740_991)).toBe('9007 TB');
});
