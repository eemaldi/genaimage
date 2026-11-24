import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import { writable } from 'svelte/store';
import type {
  ConnectionStatus,
  EventEnvelope,
  ResyncSignal,
} from '../../lib/api/events';
import type { components } from '../../lib/api/schema';
import { buildGenerateUrl, encodeFormState } from './share';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeAll, beforeEach, expect, test, vi } from 'vitest';
import {
  ApiProblemError,
  BindingFloat,
  createApiClient,
  type api,
} from '../../lib/api/client';
import Page from './Page.svelte';
import type { Parameter, Workflow } from './model';

const fake = vi.hoisted(() => ({
  listWorkflows: vi.fn<typeof api.listWorkflows>(),
  getWorkflow: vi.fn<typeof api.getWorkflow>(),
  getSettings: vi.fn<typeof api.getSettings>(),
  upload: vi.fn<typeof api.upload>(),
  validateWorkflow: vi.fn<typeof api.validateWorkflow>(),
  createRun: vi.fn<typeof api.createRun>(),
  getRun: vi.fn<typeof api.getRun>(),
  listOutputs: vi.fn<typeof api.listOutputs>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

const streams = vi.hoisted(() => ({
  listeners: new Map<string, (event: EventEnvelope) => void>(),
}));
vi.mock('../../lib/api/events', () => ({
  events: {
    status: writable<ConnectionStatus>('open'),
    resync: writable<ResyncSignal | null>(null),
    on(type: string, callback: (event: EventEnvelope) => void) {
      streams.listeners.set(type, callback);
      return () => streams.listeners.delete(type);
    },
  },
}));
import { events } from '../../lib/api/events';

const seed = 18_446_744_073_709_551_615n;
const uploadId = `sha256:${'a'.repeat(64)}`;
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
  id: 'portrait',
  version: '1',
  schema_version: 2,
  title: 'Portrait workflow',
  workflow: 'portrait.json',
  outputs: [],
  parameters: {
    Prompt: parameter('string', {
      widget: 'text',
      group: 'Prompts',
      required: true,
      placeholder: 'Describe the subject',
    }),
    Notes: parameter('string', {
      widget: 'textarea',
      group: 'Prompts',
      default: '',
    }),
    Count: parameter('integer', {
      widget: 'number',
      advanced: true,
      group: 'Tuning',
      minimum: 1,
      maximum: 5,
    }),
    Strength: parameter('number', {
      widget: 'slider',
      minimum: 0,
      maximum: 1,
      step: 0.25,
      default: 0.5,
    }),
    Style: parameter('string', {
      widget: 'select',
      options: ['ink'],
      allowed_values: ['ink', 'pencil'],
    }),
    Seed: parameter('seed', {
      widget: 'number',
      minimum: 0,
      maximum: Number(seed),
      default: seed,
    }),
    Reference: parameter('image_upload', { widget: 'file' }),
    Tiling: parameter('boolean', { widget: 'checkbox' }),
    Reserved: parameter('number', { required: false, declared_required: null }),
  },
  presets: {
    Sketch: { Prompt: 'Trees', Strength: 0.25, Style: 'pencil', Seed: 42 },
  },
};

// Keep the first DOM assertion independent of Vite's cold module transform.
beforeAll(async () => {
  await import('./Generate.svelte');
});

const available = { status: 'available', missing_classes: [] } as const;
const time = '2026-11-24T12:00:00.000Z';
const run: components['schemas']['RunResponse'] = {
  id: 'run-1',
  workflow_id: 'portrait',
  workflow_version: '1',
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
  content_type: 'video/mp4',
  created_at: time,
  source_filename: 'result.mp4',
  byte_size: 123,
  workflow_id: 'portrait',
  node_id: '7',
  kind: 'video',
  sha256: 'hash',
  tags: [],
};

beforeEach(() => {
  vi.resetAllMocks();
  history.replaceState(null, '', '/');
  fake.getWorkflow.mockResolvedValue(workflow);
  fake.validateWorkflow.mockResolvedValue({
    valid: true,
    parameter_errors: [],
    node_check: { ...available, missing_classes: [] },
  });
  fake.createRun.mockResolvedValue(run);
  fake.getRun.mockResolvedValue(run);
  fake.listOutputs.mockResolvedValue({ outputs: [] });
  fake.getSettings.mockResolvedValue({
    config_file: '',
    restart_required: [],
    settings: {
      'server.max_upload_bytes': {
        value: 4096,
        active_value: 2048,
        source: 'default',
        read_only: true,
      },
    },
  });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mount(query = 'workflow=portrait') {
  return render(Page, { params: {}, query: new URLSearchParams(query) });
}
const control = (name: string) =>
  screen.getByLabelText<HTMLInputElement>(name, { exact: true });
const reviewButton = () =>
  screen.getByRole('button', { name: 'Review parameters' });
const review = () => screen.getByRole('region', { name: 'Review parameters' });
function errorFor(name: string) {
  expect(control(name).getAttribute('aria-invalid')).toBe('true');
  return control(name)
    .getAttribute('aria-describedby')
    ?.split(' ')
    .map((id) => document.getElementById(id)?.textContent)
    .join(' ');
}

async function load() {
  mount();
  await screen.findByRole('heading', { name: workflow.title });
  return userEvent.setup();
}

test('offers an explicit picker and follows a changed workflow query', async () => {
  fake.listWorkflows.mockResolvedValue({
    workflows: [{ id: 'portrait/copy', name: 'Portrait copy', version: '1' }],
    diagnostics: [],
  });
  const { rerender } = mount('');
  const user = userEvent.setup();
  await user.selectOptions(
    await screen.findByLabelText('Workflow'),
    'portrait/copy',
  );
  expect(fake.getWorkflow).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: 'Open workflow' }));
  expect(location.hash).toBe('#/generate?workflow=portrait%2Fcopy');
  await rerender({
    params: {},
    query: new URLSearchParams(location.hash.split('?')[1]),
  });
  await screen.findByRole('heading', { name: workflow.title });
  expect(fake.getWorkflow).toHaveBeenCalledWith(
    'portrait/copy',
    expect.objectContaining({ signal: expect.any(AbortSignal) }),
  );
});

test('renders the manifest widgets and groups without inventing missing defaults', async () => {
  const user = await load();
  expect(control('Prompt').value).toBe('');
  expect(control('Prompt').required).toBe(true);
  expect(control('Prompt').placeholder).toBe('Describe the subject');
  expect(control('Notes').tagName).toBe('TEXTAREA');
  expect(screen.getByText('Manifest default: (empty text)')).toBeTruthy();
  expect(
    screen
      .getByRole('slider', { name: 'Strength slider' })
      .getAttribute('step'),
  ).toBe('0.25');
  expect(control('Style').tagName).toBe('SELECT');
  expect(control('Style').value).toBe('');
  expect(control('Seed').value).toBe(String(seed));
  expect(control('Reference').type).toBe('file');
  expect(control('Tiling').indeterminate).toBe(true);
  const advanced = screen.getByText('Advanced parameters').closest('details');
  expect(advanced?.open).toBe(false);
  await user.click(screen.getByText('Advanced parameters'));
  expect(advanced?.open).toBe(true);
  expect(within(advanced!).getByRole('group', { name: 'Tuning' })).toBeTruthy();
  expect(control('Count').value).toBe('');
  expect(control('Reserved').required).toBe(false);

  await user.click(reviewButton());
  expect(errorFor('Prompt')).toContain('Enter a value');
  expect(document.activeElement).toBe(control('Prompt'));
  await user.type(control('Prompt'), 'A lighthouse');
  await user.click(reviewButton());
  expect(within(review()).getByText(String(seed))).toBeTruthy();
  expect(within(review()).getByText('Count')).toBeTruthy();
  expect(within(review()).getByText('Tiling')).toBeTruthy();
  expect(
    within(review()).getAllByText('Not set — not submitted'),
  ).not.toHaveLength(0);
});

test('attaches numeric and enum errors, accepts off-step values, and keeps cleared defaults explicit', async () => {
  fake.getWorkflow.mockResolvedValue({
    ...workflow,
    parameters: {
      ...workflow.parameters,
      Style: parameter('string', {
        widget: 'text',
        allowed_values: ['ink', 'pencil'],
      }),
    },
  });
  const user = await load();
  await user.type(control('Prompt'), 'Test');
  await fireEvent.input(control('Count'), { target: { value: '2.5' } });
  await user.type(control('Style'), 'oil');
  await user.clear(control('Strength'));
  await user.click(reviewButton());
  expect(errorFor('Count')).toContain('whole number');
  expect(errorFor('Style')).toContain('Choose an allowed value');
  expect(errorFor('Strength')).toContain('reset to the manifest default');
  expect(screen.getByText('Advanced parameters').closest('details')?.open).toBe(
    true,
  );
  await fireEvent.input(control('Count'), { target: { value: '6' } });
  expect(errorFor('Count')).toContain('at most 5');
  await fireEvent.input(control('Count'), { target: { value: '0' } });
  expect(errorFor('Count')).toContain('at least 1');
  await fireEvent.input(control('Count'), { target: { value: '3' } });
  await fireEvent.input(control('Strength'), { target: { value: '0.3' } });
  await fireEvent.input(control('Style'), { target: { value: 'ink' } });
  await user.click(reviewButton());
  expect(within(review()).getByText('0.3')).toBeTruthy();
});

test('fills a preset explicitly, highlights changed fields and invalidates the review on edit', async () => {
  const user = await load();
  expect(screen.queryByText('Changed by preset')).toBeNull();
  await user.type(control('Notes'), 'Keep this');
  await user.selectOptions(control('Preset'), 'Sketch');
  expect(control('Prompt').value).toBe('Trees');
  expect(control('Notes').value).toBe('Keep this');
  expect(control('Seed').value).toBe('42');
  expect(screen.getAllByText('Changed by preset')).toHaveLength(4);
  await user.click(reviewButton());
  expect(within(review()).getByText('42')).toBeTruthy();
  await user.type(control('Prompt'), '!');
  expect(screen.getByText('Sketch (modified)')).toBeTruthy();
  expect(screen.getAllByText('Changed by preset')).toHaveLength(3);
  expect(
    screen.queryByRole('region', { name: 'Review parameters' }),
  ).toBeNull();
});

test('chooses an exact random seed only on review and restores the untouched locked draft', async () => {
  const random = vi
    .spyOn(crypto, 'getRandomValues')
    .mockImplementation((array) => {
      if (array instanceof Uint8Array) array.fill(0xff);
      return array;
    });
  const user = await load();
  await user.type(control('Prompt'), 'Test');
  await fireEvent.input(control('Seed'), {
    target: { value: '18446744073709551614' },
  });
  await user.click(screen.getByRole('radio', { name: 'Randomise seed' }));
  expect(random).not.toHaveBeenCalled();
  await user.click(reviewButton());
  expect(random).toHaveBeenCalledTimes(1);
  expect(within(review()).getByText(String(seed))).toBeTruthy();
  await user.click(screen.getByRole('radio', { name: 'Lock seed' }));
  expect(control('Seed').value).toBe('18446744073709551614');
  await user.click(reviewButton());
  expect(within(review()).getByText('18446744073709551614')).toBeTruthy();
  expect(random).toHaveBeenCalledTimes(1);
});

test('checks file signatures and the active size limit, then shows problem details and previews the stored ID', async () => {
  const drawImage = vi.fn();
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
    drawImage,
  } as unknown as CanvasRenderingContext2D);
  vi.stubGlobal(
    'createImageBitmap',
    vi.fn().mockResolvedValue({ width: 10, height: 10, close: vi.fn() }),
  );
  fake.upload.mockRejectedValueOnce(
    new ApiProblemError({
      status: 413,
      title: 'Too large',
      type: 'about:blank',
      detail: 'Multipart body is too large.',
      recovery: 'Use a smaller image.',
    }),
  );
  fake.upload.mockResolvedValueOnce({
    upload_id: uploadId,
    sha256: 'a'.repeat(64),
    byte_size: 8,
    media_type: 'image/png',
    original_filename: 'reference.png',
  });
  const user = await load();
  await screen.findByText(/Upload request limit: 2,048 bytes/);
  await fireEvent.change(control('Reference'), {
    target: { files: [new File(['bad'], 'bad.png', { type: 'image/png' })] },
  });
  await screen.findByRole('alert');
  expect(errorFor('Reference')).toContain('Choose a PNG, JPEG or WebP image');
  await fireEvent.change(control('Reference'), {
    target: { files: [new File([new Uint8Array(2048)], 'large.png')] },
  });
  await waitFor(() =>
    expect(errorFor('Reference')).toContain('Choose a smaller image'),
  );
  expect(fake.upload).not.toHaveBeenCalled();
  const image = new File(
    [Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10)],
    'reference.png',
  );
  await fireEvent.change(control('Reference'), { target: { files: [image] } });
  expect(
    await screen.findByText('Multipart body is too large.', {
      selector: '.ui-problem p',
    }),
  ).toBeTruthy();
  await fireEvent.change(control('Reference'), { target: { files: [image] } });
  expect(
    await screen.findByRole('img', { name: 'Preview of reference.png' }),
  ).toBeTruthy();
  await waitFor(() => expect(drawImage).toHaveBeenCalled());
  expect(screen.getByText('reference.png · image/png · 8 bytes')).toBeTruthy();
  expect(fake.upload).toHaveBeenLastCalledWith(
    image,
    expect.objectContaining({ signal: expect.any(AbortSignal) }),
  );
  await user.type(control('Prompt'), 'Test');
  await user.click(reviewButton());
  expect(within(review()).getByText(uploadId)).toBeTruthy();
});

test('reads manifest integers and writes the reviewed seed as an exact JSON number on both API paths', async () => {
  const wire = String(seed);
  const fetcher = vi
    .fn<typeof fetch>()
    .mockImplementation(
      async (_url, options) =>
        new Response(
          options?.method === 'GET'
            ? `{"parameters":{"seed":{"default":${wire}},"scale":{"default":2.0,"maximum":8.0}},"presets":{"large":{"seed":${wire}}}}`
            : '{}',
        ),
    );
  const client = createApiClient(fetcher);
  const details = await client.getWorkflow('portrait', { exactBindings: true });
  expect(details.parameters.seed.default).toBe(seed);
  expect(details.presets.large.seed).toBe(seed);
  expect(details.parameters.scale.maximum).toBe(8);
  const scale = details.parameters.scale.default;
  expect(scale).toBeInstanceOf(BindingFloat);
  await client.validateWorkflow('portrait', { parameters: { seed, scale } });
  await client.createRun(
    { workflow: 'portrait', parameters: { seed, scale } },
    'key',
  );
  for (const [, options] of fetcher.mock.calls.slice(1)) {
    expect(options?.body).toContain(`"seed":${wire}`);
    expect(options?.body).toContain('"scale":2.0');
  }
});

const submitButton = () => screen.getByRole('button', { name: 'Submit run' });

async function applyAndReview() {
  const user = await load();
  await user.selectOptions(control('Preset'), 'Sketch');
  await user.click(reviewButton());
  await screen.findByRole('region', { name: 'Review parameters' });
  return user;
}

test('maps server errors to advanced fields and blocks submission', async () => {
  fake.validateWorkflow.mockResolvedValueOnce({
    valid: false,
    node_check: { ...available, missing_classes: [] },
    parameter_errors: [
      { parameter: 'Count', reason: 'The backend rejected this count.' },
    ],
  });
  const user = await load();
  await user.selectOptions(control('Preset'), 'Sketch');
  await user.click(reviewButton());
  await screen.findByRole('alert');
  expect(errorFor('Count')).toContain('The backend rejected this count.');
  expect(document.activeElement).toBe(control('Count'));
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('submits exactly the reviewed parameters and uses the same key after ambiguity', async () => {
  const randomUUID = vi.spyOn(crypto, 'randomUUID');
  fake.createRun.mockRejectedValueOnce(
    new ApiProblemError({
      title: 'Unavailable',
      detail: 'No acknowledgement',
      status: 503,
      recovery: 'Check the run.',
      type: 'about:blank',
      run_id: 'run-1',
    }),
  );
  const user = await applyAndReview();
  const expected = {
    Prompt: 'Trees',
    Notes: '',
    Strength: 0.25,
    Style: 'pencil',
    Seed: 42,
  };
  expect(fake.validateWorkflow).toHaveBeenCalledWith(
    'portrait',
    { parameters: expected },
    expect.any(Object),
  );
  expect(
    within(review()).getByText('Version: 1', { exact: false }),
  ).toBeTruthy();
  expect(within(review()).getByText('Preset: Sketch')).toBeTruthy();
  expect(within(review()).getByText('(default)')).toBeTruthy();
  expect(within(review()).getAllByText('(preset)')).toHaveLength(4);
  await user.click(submitButton());
  const retry = await screen.findByRole('button', {
    name: 'Retry with the same key',
  });
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  const [body, key] = fake.createRun.mock.calls[0];
  expect(body).toEqual({ workflow: 'portrait', parameters: expected });
  expect(
    screen
      .getByRole('link', { name: 'Check the run details' })
      .getAttribute('href'),
  ).toBe('#/runs/run-1');
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  await user.click(retry);
  expect(fake.createRun).toHaveBeenCalledTimes(2);
  expect(fake.createRun.mock.calls[1][0]).toBe(body);
  expect(fake.createRun.mock.calls[1][1]).toBe(key);
  expect(randomUUID).toHaveBeenCalledTimes(1);
});

test('a definitive refusal permits edits and requires a fresh review', async () => {
  fake.createRun.mockRejectedValueOnce(
    new ApiProblemError({
      title: 'Invalid run',
      detail: 'Workflow changed.',
      status: 422,
      recovery: 'Review again.',
      type: 'about:blank',
    }),
  );
  const user = await applyAndReview();
  await user.click(submitButton());
  await screen.findByText('Workflow changed.');
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  expect(screen.queryByRole('button', { name: /Retry/ })).toBeNull();
  await user.type(control('Prompt'), '!');
  await user.click(reviewButton());
  await user.click(await screen.findByRole('button', { name: 'Submit run' }));
  expect(fake.createRun.mock.calls[1][0].parameters?.Prompt).toBe('Trees!');
  expect(fake.createRun.mock.calls[1][1]).not.toBe(
    fake.createRun.mock.calls[0][1],
  );
});

function emit(event: components['schemas']['Event']) {
  streams.listeners.get(event.type)?.({
    ...event,
    id: 1,
    bus_id: 'bus',
    timestamp: time,
  });
}

test('filters run events, refetches on resync, and confirms outputs before showing success', async () => {
  const user = await applyAndReview();
  await user.click(submitButton());
  await screen.findByRole('heading', { name: 'Run progress' });
  const eventRun = {
    ...run,
    generation_mode: 'generic' as const,
    client_id: 'client',
    output_dir: 'local',
  };
  emit({
    type: 'run_progress',
    run: { ...eventRun, id: 'other', status: 'running' },
    backend_event: { event_type: 'progress', value: 3, maximum: 10, raw: {} },
  });
  expect(screen.queryByRole('progressbar')).toBeNull();
  const running = {
    ...eventRun,
    status: 'running' as const,
    updated_at: '2026-11-24T12:00:01.000Z',
  };
  emit({
    type: 'run_progress',
    run: running,
    backend_event: {
      event_type: 'progress',
      node_id: '7',
      value: 3,
      maximum: 10,
      raw: {},
    },
  });
  expect(await screen.findByRole('progressbar')).toBeTruthy();
  expect(screen.getByText(/Current node: 7/)).toBeTruthy();
  expect(fake.listOutputs).not.toHaveBeenCalled();
  const completed = {
    ...running,
    status: 'completed' as const,
    updated_at: '2026-11-24T12:00:02.000Z',
  };
  fake.getRun.mockResolvedValue(completed);
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set({
    epoch: 1,
    reason: { reason: 'cursor_unavailable' },
  });
  await screen.findByText('No outputs were reported for this run.');
  expect(fake.getRun).toHaveBeenCalledTimes(2);
  expect(screen.queryByText('Run completed with outputs.')).toBeNull();
  fake.listOutputs.mockResolvedValue({ outputs: [output] });
  const { id: _id, ...eventOutput } = output;
  emit({
    type: 'run_output',
    run_id: run.id,
    output: {
      ...eventOutput,
      local_path: 'local',
      source_subfolder: '',
      source_type: 'output',
    },
  });
  await screen.findByText('Run completed with outputs.');
  expect(
    screen.getByRole('link', { name: /result.mp4/ }).getAttribute('href'),
  ).toBe('#/gallery?output=output-1');
  expect(screen.getByText('Video')).toBeTruthy();
  (events.resync as ReturnType<typeof writable<ResyncSignal | null>>).set(null);
});

test('shares form provenance and a concrete seed without validating or submitting on restore', async () => {
  const user = await load();
  const writeText = vi
    .spyOn(navigator.clipboard, 'writeText')
    .mockResolvedValue(undefined);
  await user.selectOptions(control('Preset'), 'Sketch');
  await user.type(control('Prompt'), '!');
  await user.click(screen.getByRole('radio', { name: 'Randomise seed' }));
  await user.click(reviewButton());
  await screen.findByRole('region', { name: 'Review parameters' });
  const reviewedSeed = fake.validateWorkflow.mock.calls[0][1].parameters?.Seed;
  await user.click(screen.getByRole('button', { name: 'Copy share link' }));
  const shared = new URL(writeText.mock.calls[0][0]);
  expect(shared.hash).toContain('v=1');
  expect(shared.hash).toContain('modified=1');
  cleanup();
  fake.validateWorkflow.mockClear();
  mount(shared.hash.split('?')[1]);
  await screen.findByRole('heading', { name: workflow.title });
  expect(control('Prompt').value).toBe('Trees!');
  expect(screen.getByText('Sketch (modified)')).toBeTruthy();
  expect(screen.getByText(String(reviewedSeed))).toBeTruthy();
  expect(
    screen.getByRole<HTMLInputElement>('radio', { name: 'Randomise seed' })
      .checked,
  ).toBe(true);
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
  expect(fake.createRun).not.toHaveBeenCalled();
  await user.click(reviewButton());
  expect(fake.validateWorkflow.mock.calls[0][1].parameters?.Seed).toEqual(
    reviewedSeed,
  );
});

test('Remix locks recorded seeds and flags stale versions and unknown parameters', async () => {
  const hash = buildGenerateUrl({
    ...run,
    workflow_version: 'old',
    parameters: {
      Seed: seed,
      Prompt: 'Recorded',
      Removed: 'old value',
      Reference: uploadId,
    },
  });
  mount(hash.split('?')[1]);
  await screen.findByText(/current version is 1/);
  expect(screen.getByText(/Unknown parameter “Removed”/)).toBeTruthy();
  expect(control('Seed').value).toBe(String(seed));
  expect(screen.getByText(uploadId)).toBeTruthy();
  expect((reviewButton() as HTMLButtonElement).disabled).toBe(true);
  expect(fake.createRun).not.toHaveBeenCalled();
  const user = userEvent.setup();
  await user.click(
    screen.getByRole('button', { name: 'Discard unresolved shared values' }),
  );
  await user.click(reviewButton());
  const panel = await screen.findByRole('region', {
    name: 'Review parameters',
  });
  expect(within(panel).getByText('a'.repeat(64))).toBeTruthy();
  expect(fake.validateWorkflow.mock.calls[0][1].parameters?.Seed).toBe(seed);
});

test('preserves rejected known shared values until the user corrects them', async () => {
  const hash = encodeFormState({
    workflow: 'portrait',
    version: '1',
    fields: [['Count', { source: 'user', value: 999 }]],
  });
  mount(hash.split('?')[1]);
  await screen.findByText(/Parameter “Count”: Enter a value of at most 5/);
  expect(control('Count').value).toBe('999');
  expect((reviewButton() as HTMLButtonElement).disabled).toBe(true);
  await fireEvent.input(control('Count'), { target: { value: '3' } });
  expect(screen.queryByText(/Parameter “Count”/)).toBeNull();
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
});

test('an edit during server validation prevents a stale response from enabling submit', async () => {
  let finish!: (
    value: Awaited<ReturnType<typeof api.validateWorkflow>>,
  ) => void;
  fake.validateWorkflow.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const user = await load();
  await user.selectOptions(control('Preset'), 'Sketch');
  await user.click(reviewButton());
  await screen.findByText('Validating the workflow…');
  const signal = fake.validateWorkflow.mock.calls[0][2]?.signal;
  await user.type(control('Prompt'), '!');
  expect(signal?.aborted).toBe(true);
  finish({
    valid: true,
    parameter_errors: [],
    node_check: { ...available, missing_classes: [] },
  });
  await waitFor(() =>
    expect(screen.queryByText('Validating the workflow…')).toBeNull(),
  );
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
});

test('an accepted but uncertain backend submission points to Jobs without success or retry', async () => {
  const unknown = { ...run, status: 'unknown_submission' as const };
  fake.createRun.mockResolvedValueOnce(unknown);
  fake.getRun.mockResolvedValueOnce(unknown);
  const user = await applyAndReview();
  await user.click(submitButton());
  expect(
    (
      await screen.findByRole('link', { name: 'Reconcile in Jobs' })
    ).getAttribute('href'),
  ).toBe('#/jobs?status=unknown_submission');
  expect(screen.queryByText('Run completed with outputs.')).toBeNull();
  expect(screen.queryByRole('button', { name: /Retry/ })).toBeNull();
  expect(fake.listOutputs).not.toHaveBeenCalled();
});
