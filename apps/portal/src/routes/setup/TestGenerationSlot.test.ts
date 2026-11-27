import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { writable } from 'svelte/store';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, BindingFloat, type api } from '../../lib/api/client';
import type { ConnectionStatus, ResyncSignal } from '../../lib/api/events';
import type { components } from '../../lib/api/schema';
import type { Parameter, Workflow } from '../generate/model';
import ReviewStep from './ReviewStep.svelte';
import TestGenerationSlot from './TestGenerationSlot.svelte';
import type { TestGenerationState } from './model';

const fake = vi.hoisted(() => ({
  getWorkflow: vi.fn<typeof api.getWorkflow>(),
  validateWorkflow: vi.fn<typeof api.validateWorkflow>(),
  createRun: vi.fn<typeof api.createRun>(),
  getRun: vi.fn<typeof api.getRun>(),
  listOutputs: vi.fn<typeof api.listOutputs>(),
  putSettingsWithRevision: vi.fn(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));
vi.mock('../../lib/api/events', () => ({
  events: {
    status: writable<ConnectionStatus>('open'),
    resync: writable<ResyncSignal | null>(null),
    on: () => () => {},
  },
}));

const seed = 18_446_744_073_709_551_615n;
const parameter = (
  kind: string,
  extra: Partial<Parameter> = {},
): Parameter => ({
  kind,
  node: '1',
  input: 'value',
  description: '',
  required: false,
  ...extra,
});
const workflow: Workflow = {
  revision: 'rev-1',
  id: 'text-to-image',
  version: '1.0.0',
  schema_version: 2,
  title: 'Explicit text-to-image',
  workflow: 'api-workflow.json',
  outputs: [{ node: '9', kind: 'image' }],
  presets: {},
  parameters: {
    checkpoint: parameter('string', { required: true }),
    positive_prompt: parameter('string', { required: true }),
    negative_prompt: parameter('string', { default: 'blurry, low quality' }),
    seed: parameter('seed', { required: true, default: seed }),
    steps: parameter('integer', { required: true, default: 20 }),
    cfg: parameter('number', {
      required: true,
      default: new BindingFloat(2, '2.0') as never,
    }),
    width: parameter('integer', { default: 1024 }),
    height: parameter('integer', { default: 1024 }),
    filename_prefix: parameter('string', { default: 'genaimage' }),
    extra: parameter('string'),
  },
};
const time = '2026-11-24T12:00:00.000Z';
const run: components['schemas']['RunResponse'] = {
  id: 'run-1',
  workflow_id: 'text-to-image',
  workflow_version: '1.0.0',
  workflow_sha256: 'hash',
  created_at: time,
  updated_at: time,
  prompt_id: 'prompt-1',
  status: 'queued',
  parameters: {},
};
const output: components['schemas']['OutputResponse'] = {
  id: 'output-1',
  run_id: run.id,
  content_type: 'image/png',
  created_at: time,
  source_filename: 'test.png',
  byte_size: 1,
  workflow_id: 'text-to-image',
  node_id: '9',
  kind: 'image',
  sha256: 'hash',
  tags: [],
};
const live: TestGenerationState = {
  checkpoint: 'sdxl.safetensors',
  comfyuiUrl: 'http://127.0.0.1:8188',
  activeComfyuiUrl: 'http://127.0.0.1:8188',
  savedThisSession: true,
  configFileExists: true,
  restartPending: false,
};
const refresh = vi.fn<() => Promise<boolean>>();
const onConfirmed = vi.fn<(runId: string) => void>();

beforeEach(() => {
  vi.resetAllMocks();
  sessionStorage.clear();
  fake.getWorkflow.mockResolvedValue(workflow);
  fake.validateWorkflow.mockResolvedValue({
    valid: true,
    revision: 'rev-1',
    parameter_errors: [],
    inherited_inputs: [
      {
        parameter: 'extra',
        node: '1',
        input: 'value',
        source: { kind: 'literal', json: '"inherited"' },
      },
    ],
    node_check: { status: 'available', missing_classes: [] },
  });
  fake.createRun.mockResolvedValue(run);
  fake.getRun.mockResolvedValue(run);
  fake.listOutputs.mockResolvedValue({ outputs: [] });
  refresh.mockResolvedValue(true);
});
afterEach(() => cleanup());

function mount(
  state: Partial<TestGenerationState> = {},
  confirmedRunId?: string,
) {
  return render(TestGenerationSlot, {
    state: { ...live, ...state },
    confirmedRunId,
    refresh,
    onConfirmed,
  });
}
const validate = () =>
  screen.findByRole('button', { name: 'Validate and review' });
const runButton = () =>
  screen.findByRole('button', { name: 'Run test generation' });
async function reviewed() {
  const user = userEvent.setup();
  mount();
  await user.click(await validate());
  await runButton();
  return user;
}

test('no checkpoint blocks the run and explains the fix', async () => {
  mount({ checkpoint: undefined });
  await screen.findByText(/choose an installed checkpoint/);
  expect(
    screen.queryByRole('button', { name: 'Validate and review' }),
  ).toBeNull();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test.each([
  [
    'restart pending with an unknown active endpoint',
    { activeComfyuiUrl: undefined, restartPending: true },
  ],
  [
    'an active endpoint that differs from the chosen one',
    { activeComfyuiUrl: 'http://old:8188', restartPending: true },
  ],
])('%s blocks the run and offers Check again', async (_name, state) => {
  const user = userEvent.setup();
  mount(state);
  await screen.findByText(/restart/);
  expect(
    screen.queryByRole('button', { name: 'Validate and review' }),
  ).toBeNull();
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('a missing starter workflow blocks the run and says how to add one', async () => {
  fake.getWorkflow.mockRejectedValue(
    new ApiProblemError({ title: 'Not found', status: 404 } as never),
  );
  mount();
  await screen.findByText(/add a/);
  expect(
    screen.queryByRole('button', { name: 'Validate and review' }),
  ).toBeNull();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('shows every submitted value, inherited inputs and the exact seed before running', async () => {
  await reviewed();
  const body = fake.validateWorkflow.mock.calls[0][1];
  expect(body.workflow_revision).toBe('rev-1');
  for (const text of [
    'sdxl.safetensors',
    'blurry, low quality',
    '18446744073709551615',
    '2.0',
    '20',
    '1024',
    'genaimage',
  ])
    expect(screen.getAllByText(text).length).toBeGreaterThan(0);
  expect(screen.getByText(/connected|"inherited"/)).toBeTruthy();
  expect(screen.getByText(/not creative approval/)).toBeTruthy();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('double click sends exactly one pinned request with one idempotency key', async () => {
  const user = await reviewed();
  fake.getWorkflow.mockResolvedValue({ ...workflow, revision: 'rev-2' });
  const button = await runButton();
  await user.dblClick(button);
  await waitFor(() => expect(fake.createRun).toHaveBeenCalledTimes(1));
  const [body, key] = fake.createRun.mock.calls[0];
  expect(typeof key).toBe('string');
  expect(key.length).toBeGreaterThan(0);
  expect(body.workflow_revision).toBe('rev-1');
  expect(body.parameters?.seed).toBe(seed);
  expect(body.parameters?.cfg).toBeInstanceOf(BindingFloat);
  expect(String(body.parameters?.cfg)).toBe('2.0');
});

test('an edited prompt discards the review so a new key is only made after validation', async () => {
  const user = await reviewed();
  await user.type(screen.getByLabelText('Test prompt'), '!');
  expect(
    screen.queryByRole('button', { name: 'Run test generation' }),
  ).toBeNull();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('an ambiguous submission is retried only with the same key and body', async () => {
  fake.createRun.mockRejectedValueOnce(new TypeError('network down'));
  const user = await reviewed();
  await user.click(await runButton());
  await screen.findByText('The submission outcome is uncertain');
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  expect(
    screen.getByRole<HTMLButtonElement>('button', {
      name: 'Run test generation',
    }).disabled,
  ).toBe(true);
  await user.click(
    screen.getByRole('button', { name: 'Retry with the same key' }),
  );
  expect(fake.createRun).toHaveBeenCalledTimes(2);
  expect(fake.createRun.mock.calls[1][1]).toBe(fake.createRun.mock.calls[0][1]);
  expect(fake.createRun.mock.calls[1][0]).toEqual(
    fake.createRun.mock.calls[0][0],
  );
});

test('a reload mid-submit restores the pending request and never resubmits with a new key', async () => {
  fake.createRun.mockReturnValueOnce(new Promise(() => {}));
  const user = await reviewed();
  await user.click(await runButton());
  await waitFor(() => expect(fake.createRun).toHaveBeenCalledTimes(1));
  const key = fake.createRun.mock.calls[0][1];
  cleanup();
  mount();
  await screen.findByRole('heading', { name: 'Pending submission' });
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  expect(
    screen.queryByRole('button', { name: 'Run test generation' }),
  ).toBeNull();
  fake.createRun.mockResolvedValueOnce(run);
  await user.click(
    screen.getByRole('button', { name: 'Retry with the same key' }),
  );
  expect(fake.createRun).toHaveBeenCalledTimes(2);
  expect(fake.createRun.mock.calls[1][1]).toBe(key);
});

test('a pending submission is not retryable while a precondition fails', async () => {
  fake.createRun.mockReturnValueOnce(new Promise(() => {}));
  const user = await reviewed();
  await user.click(await runButton());
  await waitFor(() => expect(fake.createRun).toHaveBeenCalledTimes(1));
  cleanup();
  mount({ checkpoint: undefined });
  await screen.findByText(/still pending/);
  expect(screen.queryByRole('button', { name: /Retry/ })).toBeNull();
  expect(fake.createRun).toHaveBeenCalledTimes(1);
});

test('success is reported only after the backend confirms outputs', async () => {
  const user = await reviewed();
  fake.getRun.mockResolvedValue({ ...run, status: 'completed' });
  await user.click(await runButton());
  await screen.findByText('No outputs were reported for this run.');
  expect(onConfirmed).not.toHaveBeenCalled();
  expect(screen.queryByText('Run completed with outputs.')).toBeNull();
  cleanup();
  sessionStorage.clear();
  fake.listOutputs.mockResolvedValue({ outputs: [output] });
  const second = await reviewed();
  await second.click(await runButton());
  await screen.findByText('Run completed with outputs.');
  await waitFor(() => expect(onConfirmed).toHaveBeenCalledWith('run-1'));
});

test('a failed run is shown as failed and never confirmed', async () => {
  fake.getRun.mockResolvedValue({
    ...run,
    status: 'failed',
    error: 'Checkpoint missing on the server.',
  });
  const user = await reviewed();
  await user.click(await runButton());
  await screen.findByText('Checkpoint missing on the server.');
  expect(screen.getByText('Failed')).toBeTruthy();
  expect(onConfirmed).not.toHaveBeenCalled();
});

test('a confirmed run links to its details', async () => {
  mount({}, 'run-9');
  const link = await screen.findByRole('link', { name: 'View the run' });
  expect(link.getAttribute('href')).toBe('#/runs/run-9');
});

test('Save does nothing while the review step is blocked', async () => {
  render(ReviewStep, {
    entries: [
      { key: 'comfyui.base_url', label: 'ComfyUI URL', from: 'a', to: 'b' },
    ],
    patch: { 'comfyui.base_url': 'b' },
    owned: [],
    settings: { settings: {} } as never,
    choices: { llmSkipped: false },
    etag: '"1"',
    saved: false,
    unverified: [],
    onSaved: vi.fn(),
    reload: vi.fn(),
    blocked: 'unknown',
    onBlock: vi.fn(),
  });
  const button = screen.getByRole<HTMLButtonElement>('button', {
    name: 'Save configuration',
  });
  button.disabled = false;
  await fireEvent.click(button);
  expect(fake.putSettingsWithRevision).not.toHaveBeenCalled();
});
