import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { tick } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import App from '../../App.svelte';
import { api, ApiProblemError, createApiClient } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import Page from './Page.svelte';
import { mockNativeDialog } from '../../test/dialog';

type Summary = components['schemas']['WorkflowSummary'];
type Details = components['schemas']['WorkflowDetails'];
const summaries: Summary[] = [
  {
    id: 'one & two',
    name: 'Portrait',
    version: '1',
    revision: 'portrait-revision',
    description: 'Human subject',
    tags: ['people', 'photo'],
    category: 'Image',
    thumbnail_url: '/declared',
  },
  {
    id: 'landscape',
    name: 'Landscape',
    version: '2',
    revision: 'landscape-revision',
    description: 'Outdoor scene',
    tags: ['nature'],
    category: 'Image',
  },
  {
    id: 'motion',
    name: 'Motion',
    version: '1',
    revision: 'motion-revision',
    description: 'Moving scene',
    tags: ['people'],
    category: 'Video',
  },
];
const detail: Details = {
  id: 'one & two',
  title: 'Portrait',
  version: '1',
  revision: 'portrait-revision',
  schema_version: 2,
  workflow: 'api.json',
  parameters: {
    prompt: {
      node: '1',
      input: 'text',
      kind: 'string',
      required: true,
      declared_required: true,
      description: 'Approved prompt',
      default: null,
    },
    seed: {
      node: '2',
      input: 'seed',
      kind: 'seed',
      required: false,
      declared_required: null,
      description: '',
      default: 0,
      widget: 'slider',
      group: 'Sampling',
      advanced: true,
      minimum: 0,
      maximum: 100,
      step: 2,
      options: [0, 2],
      allowed_values: [0, 2],
      placeholder: 'Seed',
    },
  },
  presets: { test: { seed: 0, prompt: 'hello', large: 9007199254740992 } },
  outputs: [
    { node: '3', kind: 'image', declared_kind: null },
    { node: '4', kind: 'image', declared_kind: 'image' },
  ],
};
const result = {
  revision: 'portrait-revision',
  valid: false,
  parameter_errors: [{ parameter: 'prompt', reason: 'Required' }],
  node_check: {
    status: 'unavailable' as const,
    missing_classes: [],
    reason: 'Offline',
  },
};
const issue = {
  type: 'about:blank',
  title: 'Unavailable',
  status: 503,
  detail: 'Backend offline.',
  recovery: 'Start the local server.',
};
mockNativeDialog();
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.location.hash = '';
});

it('filters from a shareable hash, searches descriptions and tags, and handles broken thumbnails', async () => {
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: summaries,
    diagnostics: [],
  });
  window.location.hash = '#/workflows?tag=people&category=Image';
  const user = userEvent.setup();
  const { container } = render(App);
  expect(await screen.findByRole('link', { name: 'Portrait' })).toBeTruthy();
  expect(screen.queryByRole('link', { name: 'Motion' })).toBeNull();
  const image = container.querySelector('img')!;
  expect(image.getAttribute('loading')).toBe('lazy');
  expect(image.getAttribute('src')).toBe(
    '/api/v1/workflows/one%20%26%20two/thumbnail',
  );
  await fireEvent.error(image);
  expect(screen.getByText('No thumbnail available')).toBeTruthy();
  await user.selectOptions(screen.getByLabelText('Category'), '');
  expect(screen.getByRole('link', { name: 'Motion' })).toBeTruthy();
  await user.type(screen.getByLabelText('Search workflows'), 'photo');
  expect(screen.queryByRole('link', { name: 'Motion' })).toBeNull();
  expect(new URLSearchParams(window.location.hash.split('?')[1]).get('q')).toBe(
    'photo',
  );
  await user.clear(screen.getByLabelText('Search workflows'));
  await user.selectOptions(screen.getByLabelText('Tag'), 'nature');
  await user.type(screen.getByLabelText('Search workflows'), 'outdoor');
  expect(screen.getByRole('link', { name: 'Landscape' })).toBeTruthy();
  expect(screen.queryByRole('link', { name: 'Portrait' })).toBeNull();
  await user.type(screen.getByLabelText('Search workflows'), 'zzz');
  expect(screen.getByText(/No workflows match/)).toBeTruthy();
  window.location.hash = '#/workflows?q=human';
  await waitFor(() =>
    expect(screen.getByRole('link', { name: 'Portrait' })).toBeTruthy(),
  );
  expect((screen.getByLabelText('Tag') as HTMLSelectElement).value).toBe('');
});

it('searches fallback titles by ID', async () => {
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: [
      { id: 'nameless-workflow', name: '', version: '1', revision: 'nameless' },
    ],
    diagnostics: [],
  });
  window.location.hash = '#/workflows';
  const user = userEvent.setup();
  render(App);
  expect(
    await screen.findByRole('link', { name: 'nameless-workflow' }),
  ).toBeTruthy();
  await user.type(screen.getByLabelText('Search workflows'), 'nameless');
  expect(screen.getByRole('link', { name: 'nameless-workflow' })).toBeTruthy();
});

it('restores edited and shared catalogue filters after viewing details', async () => {
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: summaries,
    diagnostics: [],
  });
  vi.spyOn(api, 'getWorkflow').mockResolvedValue(detail);
  vi.spyOn(api, 'validateWorkflow').mockResolvedValue(result);
  window.location.hash = '#/workflows?tag=people&category=Image&other=a%2Bb';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('link', { name: 'Portrait' });
  await user.type(screen.getByLabelText('Search workflows'), 'human');
  const expected = window.location.hash;
  await user.click(screen.getByRole('link', { name: 'Portrait' }));
  await screen.findByRole('button', { name: 'Use this workflow' });
  const back = screen.getByRole('link', { name: 'Back to workflows' });
  expect(back.getAttribute('href')).toBe(expected);
  await user.click(back);
  expect(await screen.findByRole('link', { name: 'Portrait' })).toBeTruthy();
  expect(
    (screen.getByLabelText('Search workflows') as HTMLInputElement).value,
  ).toBe('human');
  expect((screen.getByLabelText('Tag') as HTMLSelectElement).value).toBe(
    'people',
  );
  expect((screen.getByLabelText('Category') as HTMLSelectElement).value).toBe(
    'Image',
  );
  expect(window.location.hash).toBe(expected);
});

it('shows loading, empty catalogue, diagnostics and actionable errors', async () => {
  let resolve!: (value: {
    workflows: Summary[];
    diagnostics: { path: string; error: string }[];
  }) => void;
  vi.spyOn(api, 'listWorkflows').mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  render(Page, { params: {}, query: new URLSearchParams() });
  expect(screen.getByText('Loading workflows…')).toBeTruthy();
  resolve({
    workflows: [],
    diagnostics: [{ path: 'bad.json', error: 'Invalid manifest' }],
  });
  expect(await screen.findByText(/No workflows found/)).toBeTruthy();
  expect(screen.getByText(/Invalid manifest/)).toBeTruthy();
  cleanup();
  vi.mocked(api.listWorkflows).mockRejectedValue(new ApiProblemError(issue));
  render(Page, { params: {}, query: new URLSearchParams() });
  expect(await screen.findByText('Start the local server.')).toBeTruthy();
});

it('shows manifest values, validation and navigates to Generate with the exact ID', async () => {
  vi.spyOn(api, 'getWorkflow').mockResolvedValue(detail);
  vi.spyOn(api, 'validateWorkflow').mockResolvedValue(result);
  window.location.hash = '#/workflows/one%20%26%20two';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('heading', { name: 'Portrait' });
  expect(await screen.findByText('Offline')).toBeTruthy();
  expect(
    screen.getByText('Not confirmed valid with these values.'),
  ).toBeTruthy();
  expect(
    screen.getByText('prompt:', { selector: 'strong' }).parentElement
      ?.textContent,
  ).toContain('Required');
  expect(screen.getByRole('heading', { name: 'Basic' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: 'Advanced' })).toBeTruthy();
  const seed = screen
    .getByRole('heading', { name: 'seed' })
    .closest('article')!;
  expect(within(seed).getAllByText('0')).toBeTruthy();
  expect(within(seed).getByText('"slider"')).toBeTruthy();
  expect(within(seed).getAllByText('[0,2]')).toHaveLength(2);
  expect(screen.getByText('Not specified or null')).toBeTruthy();
  expect(within(seed).getByText('No (default)')).toBeTruthy();
  expect(
    within(
      screen.getByRole('heading', { name: 'Declared outputs' }).parentElement!,
    ).getAllByText(/\(default\)/),
  ).toHaveLength(1);
  expect(screen.getByRole('heading', { name: 'test' })).toBeTruthy();
  expect(screen.getByText('"hello"')).toBeTruthy();
  expect(screen.getByText('"[exact integer unavailable]"')).toBeTruthy();
  expect(screen.getByText('3')).toBeTruthy();
  expect(api.validateWorkflow).toHaveBeenCalledWith(
    'one & two',
    {},
    expect.anything(),
  );
  await user.click(screen.getByRole('button', { name: 'Use this workflow' }));
  expect(window.location.hash).toBe('#/generate?workflow=one+%26+two');
});

it('shows backend node status without JSON reviver source support', async () => {
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
  const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
    Response.json({
      ...result,
      node_check: { status: 'missing', missing_classes: ['KSampler'] },
    }),
  );
  vi.spyOn(api, 'getWorkflow').mockResolvedValue(detail);
  vi.spyOn(api, 'validateWorkflow').mockImplementation(
    createApiClient(fetcher).validateWorkflow,
  );
  window.location.hash = '#/workflows/one%20%26%20two';
  render(App);
  expect(await screen.findByText('Missing nodes: KSampler')).toBeTruthy();
  expect(fetcher).toHaveBeenCalledExactlyOnceWith(
    '/api/v1/workflows/one%20%26%20two/validate',
    expect.objectContaining({ method: 'POST', body: '{}' }),
  );
});

it('renders duplicate IDs from discovery without crashing', async () => {
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: [summaries[0], { ...summaries[0], name: 'Another manifest' }],
    diagnostics: [],
  });
  render(Page, { params: {}, query: new URLSearchParams() });
  expect(await screen.findByRole('link', { name: 'Portrait' })).toBeTruthy();
  expect(screen.getByRole('link', { name: 'Another manifest' })).toBeTruthy();
});

it('ignores a stale detail response when navigating to another workflow', async () => {
  let finishFirst!: (value: Details) => void;
  vi.spyOn(api, 'getWorkflow').mockImplementation((id) =>
    id === 'first'
      ? new Promise((resolve) => {
          finishFirst = resolve;
        })
      : Promise.resolve({ ...detail, id: 'second', title: 'Second workflow' }),
  );
  vi.spyOn(api, 'validateWorkflow').mockResolvedValue(result);
  window.location.hash = '#/workflows/first';
  render(App);
  await waitFor(() => expect(api.getWorkflow).toHaveBeenCalled());
  window.location.hash = '#/workflows/second';
  expect(
    await screen.findByRole('heading', { name: 'Second workflow' }),
  ).toBeTruthy();
  finishFirst(detail);
  await tick();
  expect(screen.queryByRole('heading', { name: 'Portrait' })).toBeNull();
  expect(api.validateWorkflow).not.toHaveBeenCalledWith(
    'first',
    expect.anything(),
    expect.anything(),
  );
});

it('keeps detail visible if validation fails and reports the recovery hint', async () => {
  vi.spyOn(api, 'getWorkflow').mockResolvedValue(detail);
  vi.spyOn(api, 'validateWorkflow').mockRejectedValue(
    new ApiProblemError(issue),
  );
  window.location.hash = '#/workflows/one';
  render(App);
  expect(await screen.findByText('Start the local server.')).toBeTruthy();
  expect(screen.getByRole('heading', { name: 'Portrait' })).toBeTruthy();
});

it('renders 100 fake workflows within the jsdom regression budget', async () => {
  const workflows = Array.from({ length: 100 }, (_, index) => ({
    id: `id-${index}`,
    name: `Workflow ${index}`,
    version: '1',
    revision: `revision-${index}`,
    tags: ['example'],
  }));
  let resolve!: (value: { workflows: Summary[]; diagnostics: [] }) => void;
  vi.spyOn(api, 'listWorkflows').mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const { container } = render(Page, {
    params: {},
    query: new URLSearchParams(),
  });
  const start = performance.now();
  resolve({ workflows, diagnostics: [] });
  await Promise.resolve();
  await tick();
  const elapsed = performance.now() - start;
  expect(container.querySelectorAll('.card')).toHaveLength(100);
  expect(elapsed).toBeLessThan(1500);
  console.info(`100-workflow jsdom render: ${elapsed.toFixed(1)} ms`);
});
