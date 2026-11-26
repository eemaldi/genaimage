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
import { encodeFormState, restoreFormState } from './share';
import { buildGenerateUrl } from '../runs/remix';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeAll, beforeEach, expect, test, vi } from 'vitest';
import {
  ApiProblemError,
  BindingFloat,
  createApiClient,
  type api,
} from '../../lib/api/client';
import App from '../../App.svelte';
import Page from './Page.svelte';
import {
  initialField,
  sameValue,
  type Parameter,
  type Workflow,
} from './model';
import { savePending } from './pending';
import { mockNativeDialog } from '../../test/dialog';

mockNativeDialog();

const fake = vi.hoisted(() => ({
  listWorkflows: vi.fn<typeof api.listWorkflows>(),
  health: vi.fn<typeof api.health>(),
  promptAssist: vi.fn<typeof api.promptAssist>(),
  suggestWorkflow: vi.fn<typeof api.suggestWorkflow>(),
  getWorkflow: vi.fn<typeof api.getWorkflow>(),
  getSettings: vi.fn<typeof api.getSettings>(),
  upload: vi.fn<typeof api.upload>(),
  validateWorkflow: vi.fn<typeof api.validateWorkflow>(),
  createRun: vi.fn<typeof api.createRun>(),
  getRun: vi.fn<typeof api.getRun>(),
  listRuns: vi.fn<typeof api.listRuns>(),
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
  revision: 'portrait-rev-1',
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

const inheritedInputs: components['schemas']['WorkflowInheritedInput'][] = [
  {
    parameter: 'Count',
    node: '1',
    input: 'value',
    source: { kind: 'literal', json: '4' },
  },
  {
    parameter: 'Reference',
    node: '1',
    input: 'value',
    source: { kind: 'connection', node: '4', output: 0 },
  },
  ...['Style', 'Tiling', 'Reserved', 'Omitted'].map((parameter) => ({
    parameter,
    node: '1',
    input: 'value',
    source: { kind: 'literal' as const, json: 'null' },
  })),
];
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
  sessionStorage.clear();
  fake.getWorkflow.mockResolvedValue(workflow);
  fake.health.mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local-text' },
  });
  fake.validateWorkflow.mockResolvedValue({
    valid: true,
    revision: workflow.revision,
    parameter_errors: [],
    inherited_inputs: inheritedInputs,
    node_check: { ...available, missing_classes: [] },
  });
  fake.createRun.mockResolvedValue(run);
  fake.getRun.mockResolvedValue(run);
  fake.listRuns.mockResolvedValue({ runs: [] });
  fake.listOutputs.mockResolvedValue({ outputs: [] });
  fake.getSettings.mockResolvedValue({
    config_file: '',
    config_file_exists: true,
    api_key_set: false,
    api_key_read_only: false,
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
    workflows: [
      {
        id: 'portrait/copy',
        name: 'Portrait copy',
        version: '1',
        revision: workflow.revision,
      },
    ],
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
  expect(within(review()).getByText(/Inherited from workflow: 4/)).toBeTruthy();
  expect(
    within(review()).getByText(
      /Inherited from workflow: connected to node 4 output 0/,
    ),
  ).toBeTruthy();
});

test('applies and undoes prompt assistance on only its target field', async () => {
  fake.promptAssist.mockResolvedValue({
    applied: false,
    mode: 'expand',
    model: 'local-text',
    proposal_count: 1,
    proposals: [
      {
        text: 'A lighthouse at blue hour',
        diff: [
          { op: 'equal', text: 'A lighthouse' },
          { op: 'insert', text: ' at blue hour' },
        ],
      },
    ],
    template: { name: 'expand', sha256: 'hash' },
  });
  const user = await load();
  await user.type(control('Prompt'), 'A lighthouse');
  await user.type(control('Notes'), 'Keep these notes');
  await user.click(screen.getByRole('button', { name: 'Expand' }));
  await user.click(await screen.findByRole('button', { name: 'Accept' }));
  expect(control('Prompt').value).toBe('A lighthouse at blue hour');
  expect(control('Notes').value).toBe('Keep these notes');
  await user.click(screen.getByRole('button', { name: 'Undo' }));
  expect(control('Prompt').value).toBe('A lighthouse');
  expect(control('Notes').value).toBe('Keep these notes');
  expect(fake.promptAssist).toHaveBeenCalledWith(
    expect.objectContaining({
      workflow_id: 'portrait',
      parameter: 'Prompt',
      prompt: 'A lighthouse',
    }),
    expect.anything(),
  );
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
    revision: workflow.revision,
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
    { parameters: expected, workflow_revision: workflow.revision },
    expect.objectContaining({ exactNumbers: true }),
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
  expect(body).toEqual({
    workflow: 'portrait',
    parameters: expected,
    workflow_revision: workflow.revision,
  });
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
  expect(shared.hash).toContain('v=2');
  expect(shared.hash).toContain(`wr=${workflow.revision}`);
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

test('Remix warns when the revision changed without a version change', async () => {
  const hash = buildGenerateUrl({
    ...run,
    workflow_revision: 'source-revision',
    parameters: { Seed: seed, Prompt: 'Recorded' },
  });
  const query = new URLSearchParams(hash.split('?')[1]);
  expect(query.get('wv')).toBe(workflow.version);
  expect(query.get('wr')).toBe('source-revision');
  expect(query.get('unverified_source')).toBeNull();
  const restored = restoreFormState(workflow, query);
  expect(restored.fields.get('Seed')?.value).toBe(seed);
  expect(restored.revisionNotice).toMatch(/revision differs/);
  mount(query.toString());
  expect(await screen.findByText(/workflow revision differs/)).toBeTruthy();
  expect(control('Prompt').value).toBe('Recorded');
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('legacy Remix warns that the source workflow cannot be verified', async () => {
  const hash = buildGenerateUrl({
    ...run,
    parameters: { Seed: seed, Prompt: 'Recorded' },
  });
  const query = new URLSearchParams(hash.split('?')[1]);
  expect(query.get('wr')).toBeNull();
  expect(query.get('unverified_source')).toBe('1');
  const restored = restoreFormState(workflow, query);
  expect(restored.unverifiedSource).toBe(true);
  const reshared = encodeFormState({
    workflow: workflow.id,
    version: workflow.version,
    revision: workflow.revision,
    fields: restored.fields,
    unverifiedSource: restored.unverifiedSource,
  });
  expect(
    restoreFormState(workflow, new URLSearchParams(reshared.split('?')[1]))
      .unverifiedSource,
  ).toBe(true);
  mount(query.toString());
  expect(
    await screen.findByText(
      'Remix uses the current workflow; it cannot be verified against the source run.',
    ),
  ).toBeTruthy();
  expect(control('Prompt').value).toBe('Recorded');
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('the run-detail Remix adapter restores recorded fields without submitting', async () => {
  const hash = buildGenerateUrl({
    ...run,
    parameters: {
      Seed: seed,
      Prompt: 'Coast  at\nnight',
      Strength: new BindingFloat(1),
      Tiling: true,
    },
  });
  const query = new URLSearchParams(hash.split('?')[1]);
  const restored = restoreFormState(workflow, query);
  expect(restored.fields.get('Seed')?.value).toBe(seed);
  expect(restored.fields.get('Prompt')?.value).toBe('Coast  at\nnight');
  expect(restored.fields.get('Strength')?.value).toEqual(new BindingFloat(1));
  mount(query.toString());
  await screen.findByRole('heading', { name: workflow.title });
  expect(control('Prompt').value).toBe('Coast  at\nnight');
  expect(control('Seed').value).toBe(String(seed));
  expect(control('Strength').value).toBe('1.0');
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
  expect(fake.createRun).not.toHaveBeenCalled();
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
    revision: workflow.revision,
    parameter_errors: [],
    inherited_inputs: inheritedInputs,
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

test('opens Generate from the catalogue and distinguishes defaulted, declared and unknown presence', async () => {
  const id = 'portrait & ink';
  fake.getWorkflow.mockResolvedValue({
    ...workflow,
    id,
    parameters: {
      Omitted: parameter('string', { declared_required: null, default: null }),
      Declared: parameter('boolean', {
        declared_required: false,
        default: false,
      }),
      Unknown: parameter('string', { required: true }),
    },
    outputs: [
      { node: '1', kind: 'image', declared_kind: null },
      { node: '2', kind: 'image', declared_kind: 'image' },
      { node: '3', kind: 'image' },
    ],
  });
  history.replaceState(null, '', `#/workflows/${encodeURIComponent(id)}`);
  render(App);
  const user = userEvent.setup();
  await user.click(
    await screen.findByRole('button', { name: 'Use this workflow' }),
  );
  await screen.findByRole('heading', { name: 'Generate' });
  expect(location.hash).toBe('#/generate?workflow=portrait+%26+ink');
  await screen.findByLabelText('Omitted');
  expect(fake.getWorkflow).toHaveBeenLastCalledWith(
    id,
    expect.objectContaining({ exactBindings: true }),
  );
  expect(control('Omitted').value).toBe('');
  expect(control('Omitted').required).toBe(false);
  expect(control('Unknown').required).toBe(true);
  expect(screen.getByText('Optional (default).')).toBeTruthy();
  expect(screen.getByText('Optional (declared).')).toBeTruthy();
  expect(screen.getByText('Required (declaration unavailable).')).toBeTruthy();
  expect(screen.getByText('Manifest default: false')).toBeTruthy();
  await user.type(control('Unknown'), 'Explicit value');
  await user.click(reviewButton());
  const panel = within(
    await screen.findByRole('region', { name: 'Review parameters' }),
  );
  expect(panel.getByText('· Optional (default)')).toBeTruthy();
  expect(panel.getByText('· Optional (declared)')).toBeTruthy();
  expect(panel.getByText('· Required (declaration unavailable)')).toBeTruthy();
  expect(panel.getByText(/Inherited from workflow: null/)).toBeTruthy();
  expect(panel.getByText('(default)')).toBeTruthy();
  expect(panel.getByText(/image \(default\)/)).toBeTruthy();
  expect(panel.getByText(/image \(declared\)/)).toBeTruthy();
  expect(panel.getByText(/image \(declaration unavailable\)/)).toBeTruthy();
  expect(fake.validateWorkflow).toHaveBeenLastCalledWith(
    id,
    {
      parameters: { Declared: false, Unknown: 'Explicit value' },
      workflow_revision: workflow.revision,
    },
    expect.any(Object),
  );
  expect(fake.createRun).not.toHaveBeenCalled();
});

test('recovers the exact submission after Jobs and reload even when the workflow is gone', async () => {
  const uuid = vi.spyOn(crypto, 'randomUUID');
  fake.createRun.mockRejectedValueOnce(new TypeError('Lost response'));
  history.replaceState(null, '', '#/generate?workflow=portrait');
  render(App);
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText('Prompt'), 'Pending');
  await user.click(reviewButton());
  await user.click(await screen.findByRole('button', { name: 'Submit run' }));
  const [body, key] = fake.createRun.mock.calls[0];
  expect(body.workflow_revision).toBe(workflow.revision);
  await user.click(
    await screen.findByRole('link', { name: 'check status in Jobs' }),
  );
  await screen.findByRole('heading', { name: 'Jobs' });
  fake.getWorkflow.mockRejectedValue(
    new ApiProblemError({
      status: 404,
      type: 'about:blank',
      title: 'Workflow not found',
      detail: 'The workflow was removed.',
      recovery: 'Choose another workflow.',
    }),
  );
  location.hash = '#/generate?workflow=portrait';
  await screen.findByRole('heading', { name: 'Pending submission' });
  expect(screen.getByText(workflow.revision)).toBeTruthy();
  await screen.findByText('Workflow not found');
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  cleanup();
  render(App);
  expect(await screen.findByText(String(seed))).toBeTruthy();
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  await user.click(
    screen.getByRole('button', { name: 'Retry with the same key' }),
  );
  expect(fake.createRun.mock.calls[1].slice(0, 2)).toEqual([body, key]);
  expect(uuid).toHaveBeenCalledTimes(1);
  await screen.findByRole('heading', { name: 'Run progress' });
  expect(sessionStorage.length).toBe(0);
});

test.each(['1', 'old'])(
  'keeps shared defaults and blocks a newly defaulted unset field (version %s)',
  async (version) => {
    fake.getWorkflow.mockResolvedValue({
      ...workflow,
      outputs: [],
      parameters: {
        Strength: parameter('number', { default: 0.75 }),
        Notes: parameter('string', { default: 'new default' }),
      },
    });
    const hash = encodeFormState({
      workflow: 'portrait',
      version,
      fields: [
        ['Strength', { value: 0.5, source: 'default' }],
        ['Notes', {}],
      ],
    });
    mount(hash.split('?')[1]);
    await screen.findByText(
      /Shared default: 0.5. Current manifest default: 0.75/,
    );
    expect(control('Strength').value).toBe('0.5');
    expect(control('Notes').value).toBe('');
    expect(screen.getByText(/explicitly left this unset/)).toBeTruthy();
    expect((reviewButton() as HTMLButtonElement).disabled).toBe(true);
    expect(fake.validateWorkflow).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(
      screen.getByRole('button', { name: 'Discard unresolved shared values' }),
    );
    await user.click(reviewButton());
    expect(fake.validateWorkflow.mock.calls[0][1].parameters).toEqual({
      Strength: 0.5,
      Notes: 'new default',
    });
    expect(within(review()).getByText('(shared default)')).toBeTruthy();
  },
);

test('pins both requests and reloads a replacement workflow before allowing a new submission', async () => {
  const user = await applyAndReview();
  expect(within(review()).getByText(workflow.revision)).toBeTruthy();
  expect(fake.validateWorkflow.mock.lastCall?.[1].workflow_revision).toBe(
    workflow.revision,
  );
  const next = {
    ...workflow,
    revision: 'portrait-rev-2',
    parameters: {
      ...workflow.parameters,
      Notes: { ...workflow.parameters.Notes, default: 'New manifest default' },
    },
  };
  fake.getWorkflow.mockResolvedValueOnce(next);
  fake.createRun.mockRejectedValueOnce(
    new ApiProblemError({
      type: 'urn:genaimage:problem:workflow-changed',
      status: 409,
      title: 'Workflow changed',
      detail: 'The reviewed revision is no longer current.',
      recovery: 'Reload and review again.',
    }),
  );
  await user.click(submitButton());
  await screen.findByText('The reviewed revision is no longer current.');
  await screen.findByText(/Current manifest default: "New manifest default"/);
  expect(
    screen.getByText(/revision differs from the one in this link/),
  ).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  expect(sessionStorage.length).toBe(0);
  expect(fake.getWorkflow).toHaveBeenCalledTimes(2);
  expect(fake.createRun).toHaveBeenCalledTimes(1);
  const [original, originalKey] = fake.createRun.mock.calls[0];
  expect(original.workflow_revision).toBe(workflow.revision);
  fake.validateWorkflow.mockResolvedValueOnce({
    revision: next.revision,
    valid: true,
    parameter_errors: [],
    inherited_inputs: inheritedInputs,
    node_check: { ...available, missing_classes: [] },
  });
  await user.click(reviewButton());
  await user.click(await screen.findByRole('button', { name: 'Submit run' }));
  expect(fake.validateWorkflow.mock.lastCall?.[1].workflow_revision).toBe(
    next.revision,
  );
  const [submitted, key] = fake.createRun.mock.calls[1];
  expect(submitted.workflow_revision).toBe(next.revision);
  expect(submitted.parameters?.Notes).toBe('');
  expect(key).not.toBe(originalKey);
});

test('loads the replacement if the revision changes before validation', async () => {
  const user = await load();
  await user.type(screen.getByLabelText('Prompt'), 'Keep this prompt');
  fake.getWorkflow.mockResolvedValueOnce({
    ...workflow,
    revision: 'new-revision',
  });
  fake.validateWorkflow.mockRejectedValueOnce(
    new ApiProblemError({
      type: 'urn:genaimage:problem:workflow-changed',
      status: 409,
      title: 'Workflow changed',
      detail: 'The loaded revision is no longer current.',
      recovery: 'Reload and review again.',
    }),
  );
  await user.click(reviewButton());
  expect((await screen.findByRole('alert')).textContent).toContain(
    'Workflow changed',
  );
  expect((await screen.findByLabelText<HTMLInputElement>('Prompt')).value).toBe(
    'Keep this prompt',
  );
  expect(fake.getWorkflow).toHaveBeenCalledTimes(2);
  expect(fake.createRun).not.toHaveBeenCalled();
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
});

test('dismisses an unpinned pending submission without a POST and unlocks editing', async () => {
  savePending({
    key: 'old-key',
    version: '1',
    body: {
      workflow: 'portrait',
      parameters: { Prompt: 'Previously submitted' },
    },
  });
  const user = await load();
  const prompt = control('Prompt');
  expect(prompt.matches(':disabled')).toBe(true);
  expect(
    screen.queryByRole('button', { name: 'Retry with the same key' }),
  ).toBeNull();
  await user.click(
    screen.getByRole('button', {
      name: "I've checked Jobs — dismiss this pending submission",
    }),
  );
  expect(prompt.matches(':disabled')).toBe(false);
  expect(sessionStorage.length).toBe(0);
  expect(fake.createRun).not.toHaveBeenCalled();
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
});

test('flags changed inherited inputs from a reviewed share link without submitting them', async () => {
  const user = await applyAndReview();
  const writeText = vi.spyOn(navigator.clipboard, 'writeText');
  await user.click(screen.getByRole('button', { name: 'Copy share link' }));
  const link = new URL(writeText.mock.calls[0][0]);
  cleanup();
  fake.validateWorkflow.mockResolvedValueOnce({
    valid: true,
    revision: workflow.revision,
    parameter_errors: [],
    node_check: { ...available, missing_classes: [] },
    inherited_inputs: inheritedInputs.map((entry) =>
      entry.parameter === 'Count'
        ? { ...entry, source: { kind: 'literal', json: '5' } }
        : entry,
    ),
  });
  mount(link.hash.split('?')[1]);
  await screen.findByLabelText('Prompt');
  await user.click(reviewButton());
  expect(
    within(review()).getByText(/Inherited inputs differ from the saved link/),
  ).toBeTruthy();
  expect(
    within(review()).getByText(
      /Count: The saved link inherited 4.*current workflow inherits 5/,
    ),
  ).toBeTruthy();
  await user.click(submitButton());
  expect(fake.createRun.mock.lastCall?.[0].parameters).not.toHaveProperty(
    'Count',
  );
  expect(fake.createRun.mock.lastCall?.[0].parameters).not.toHaveProperty(
    'Reference',
  );
});

test('round-trips signed zero from the manifest through a link and both request bodies', async () => {
  const fetcher = vi
    .fn<typeof fetch>()
    .mockImplementation(
      async (_url, options) =>
        new Response(
          options?.method === 'GET'
            ? '{"parameters":{"scale":{"kind":"number","default":-0.0}},"presets":{}}'
            : '{}',
        ),
    );
  const client = createApiClient(fetcher);
  const details = await client.getWorkflow('portrait', { exactBindings: true });
  const field = initialField(details.parameters.scale);
  expect(sameValue(field.value, new BindingFloat(0))).toBe(false);
  const hash = encodeFormState({
    workflow: 'portrait',
    fields: [['scale', field]],
  });
  const query = new URLSearchParams(hash.split('?')[1]);
  expect(query.get('p')).toContain('["float","-0.0"]');
  const restored = restoreFormState(details, query);
  expect(restored.warnings).toEqual([]);
  const value = restored.fields.get('scale')?.value;
  expect(value).toBeInstanceOf(BindingFloat);
  expect(Object.is((value as BindingFloat).value, -0)).toBe(true);
  const parameters = { scale: value };
  await client.validateWorkflow('portrait', { parameters });
  await client.createRun({ workflow: 'portrait', parameters }, 'key');
  for (const [, request] of fetcher.mock.calls.slice(1)) {
    expect(request?.body).toContain('"scale":-0.0');
  }
});

test('blocks review of whole-valued floats if revivers cannot read the original JSON token', async () => {
  const parse = JSON.parse;
  vi.spyOn(JSON, 'parse').mockImplementation((text, reviver) =>
    parse(
      text,
      reviver &&
        function (this: unknown, key, value: unknown) {
          return reviver.call(this, key, value);
        },
    ),
  );
  const manifest = JSON.stringify({
    ...workflow,
    parameters: { Scale: parameter('number', { default: 'replace-me' }) },
  }).replace('"replace-me"', '2.0');
  const fetcher = vi
    .fn<typeof fetch>()
    .mockImplementation(async () => new Response(manifest));
  const client = createApiClient(fetcher);
  fake.getWorkflow.mockImplementation(client.getWorkflow);
  mount();
  expect((await screen.findByRole('alert')).textContent).toContain(
    'Your browser cannot read numbers exactly; update to a current Chromium, Firefox or Safari release.',
  );
  expect(
    screen.queryByRole('button', { name: 'Review parameters' }),
  ).toBeNull();
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  expect(fake.validateWorkflow).not.toHaveBeenCalled();
  expect(fake.createRun).not.toHaveBeenCalled();
  await expect(
    client.getRecordedRun('run-1', { exactBindings: true }),
  ).rejects.toThrow('cannot read numbers exactly');
  await expect(client.getWorkflow('portrait')).resolves.toHaveProperty(
    'parameters.Scale.default',
    2,
  );
  await expect(
    client.validateWorkflow(
      'portrait',
      { parameters: { Scale: 2 } },
      { exactNumbers: true },
    ),
  ).rejects.toThrow('cannot read numbers exactly');
  await expect(
    client.createRun({ workflow: 'portrait', parameters: { Scale: 2 } }, 'key'),
  ).rejects.toThrow('cannot read numbers exactly');
  expect(fetcher.mock.calls.map(([, request]) => request?.method)).toEqual([
    'GET',
    'GET',
    'GET',
  ]);
});
