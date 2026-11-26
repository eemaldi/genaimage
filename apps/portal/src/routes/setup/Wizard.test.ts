import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, type api } from '../../lib/api/client';
import Wizard from './Wizard.svelte';
import Page from './Page.svelte';
import { isDismissed, rememberDismissal } from './model';

const fake = vi.hoisted(() => ({
  setupStatus: vi.fn(),
  setupDetect: vi.fn(),
  getSettingsWithRevision: vi.fn(),
  putSettingsWithRevision: vi.fn(),
  testSettings: vi.fn(),
  llmModels: vi.fn(),
  checkpoints: vi.fn(),
  doctor: vi.fn(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake as unknown as typeof api,
}));

type Source = { source: 'default' } | { source: 'env'; variable: string };
const setting = (value: unknown, source: Source = { source: 'default' }) => ({
  ...source,
  value,
  read_only: source.source === 'env',
});

function status(over: Record<string, unknown> = {}) {
  return {
    needs_setup: true,
    reasons: [
      {
        code: 'no_config_file',
        summary: 'No configuration file exists yet.',
        blocking: true,
        fix: 'Save the configuration from this wizard.',
      },
    ],
    config_file: '/home/u/.config/genaimage/config.toml',
    config_file_exists: false,
    endpoints_env_owned: false,
    restart_required: [],
    comfyui: { configured_url: 'http://127.0.0.1:8188', source: 'default' },
    llm: { configured_url: 'http://127.0.0.1:11434/v1', source: 'default' },
    ...over,
  };
}
function snapshot(compose = false, over: Record<string, unknown> = {}) {
  const env = (name: string): Source => ({ source: 'env', variable: name });
  return {
    etag: '"rev-1"',
    settings: {
      config_file: '/home/u/.config/genaimage/config.toml',
      config_file_exists: false,
      restart_required: [],
      api_key_set: false,
      api_key_read_only: false,
      settings: {
        'comfyui.base_url': setting(
          'http://127.0.0.1:8188',
          compose ? env('GENAIMAGE_COMFYUI_URL') : undefined,
        ),
        'llm.base_url': setting(
          'http://127.0.0.1:11434/v1',
          compose ? env('GENAIMAGE_LLM_URL') : undefined,
        ),
        'llm.text_model': setting(''),
        'llm.vision_model': setting(''),
      },
      ...over,
    },
  };
}
const services = [
  {
    kind: 'comfyui',
    label: 'ComfyUI on this machine',
    url: 'http://127.0.0.1:8188',
    configured: true,
    status: 'found',
    detail: 'ComfyUI answered.',
    version: '0.3.1',
  },
  {
    kind: 'comfyui',
    label: 'ComfyUI on the Compose network',
    url: 'http://comfyui:8188',
    configured: false,
    status: 'skipped',
    detail: 'Host comfyui is not allow-listed.',
    fix: 'Add `comfyui` to `comfyui.allowed_hosts`.',
  },
  {
    kind: 'llm',
    label: 'Ollama',
    url: 'http://127.0.0.1:11434/v1',
    configured: true,
    status: 'found',
    detail: 'Ollama answered.',
    flavour: 'ollama',
  },
  {
    kind: 'llm',
    label: 'LM Studio',
    url: 'http://127.0.0.1:1234/v1',
    configured: false,
    status: 'found',
    detail: 'LM Studio answered.',
    flavour: 'openai',
  },
];
const props = { params: {}, query: new URLSearchParams() };
const problem = (status: number, title: string) =>
  new ApiProblemError({ title, status, recovery: 'Do the thing.' } as never);

beforeEach(() => {
  fake.setupStatus.mockResolvedValue(status());
  fake.getSettingsWithRevision.mockResolvedValue(snapshot());
  fake.setupDetect.mockResolvedValue({ services });
  fake.testSettings.mockResolvedValue({
    comfyui: 'ok',
    llm: 'ok',
    llm_models: [
      { id: 'qwen3:8b', supports_tools: true, supports_vision: false },
      { id: 'llava:7b', supports_tools: false, supports_vision: true },
    ],
  });
  fake.llmModels.mockResolvedValue({
    source: 'ollama',
    pull_supported: true,
    models: [{ name: 'qwen3:8b' }, { name: 'llava:7b' }],
    roles: [],
  });
  fake.checkpoints.mockResolvedValue({
    checkpoints: ['sdxl.safetensors'],
    locations: [],
    refresh_note: '',
  });
});
afterEach(() => {
  cleanup();
  for (const mock of Object.values(fake)) mock.mockReset();
  localStorage.clear();
});

async function open() {
  const user = userEvent.setup();
  render(Wizard);
  await screen.findByRole('heading', { name: '1. Detect services' });
  return user;
}
const next = (user: ReturnType<typeof userEvent.setup>) =>
  user.click(screen.getByRole('button', { name: /^Next:/ }));

test('lazy page loads the wizard and shows why setup is needed', async () => {
  render(Page, props);
  expect(screen.getByText('Loading the setup wizard…')).toBeTruthy();
  expect(
    await screen.findByText('No configuration file exists yet.'),
  ).toBeTruthy();
  expect(screen.getByText('Blocking')).toBeTruthy();
});

test('renders detection results including the allow-list fix for skipped hosts', async () => {
  await open();
  const skipped = (
    await screen.findByText('ComfyUI on the Compose network')
  ).closest('li')!;
  expect(skipped.dataset.status).toBe('skipped');
  expect(within(skipped).getByText('How to fix:')).toBeTruthy();
  expect(within(skipped).getByText('comfyui.allowed_hosts').tagName).toBe(
    'CODE',
  );
  expect(screen.getByText('Version 0.3.1.')).toBeTruthy();
});

test('Compose shows env-owned endpoints read-only and offers no endpoint choice', async () => {
  fake.setupStatus.mockResolvedValue(
    status({
      endpoints_env_owned: true,
      comfyui: {
        configured_url: 'http://comfyui:8188',
        source: 'env',
        variable: 'GENAIMAGE_COMFYUI_URL',
      },
    }),
  );
  fake.getSettingsWithRevision.mockResolvedValue(snapshot(true));
  await open();
  expect(await screen.findAllByText(/GENAIMAGE_COMFYUI_URL/)).not.toHaveLength(
    0,
  );
  expect(screen.queryByRole('radio')).toBeNull();
  expect(screen.getAllByText(/cannot be changed here/).length).toBeGreaterThan(
    0,
  );
});

test('a stale detection response is ignored', async () => {
  let first!: (value: unknown) => void;
  fake.setupDetect.mockReturnValueOnce(
    new Promise((resolve) => (first = resolve)),
  );
  const user = await open();
  fake.setupDetect.mockResolvedValueOnce({
    services: [{ ...services[0], label: 'Newest ComfyUI' }],
  });
  await user.click(screen.getByRole('button', { name: /Detect/ }));
  await screen.findByText('Newest ComfyUI');
  first({ services: [{ ...services[0], label: 'Stale ComfyUI' }] });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(screen.queryByText('Stale ComfyUI')).toBeNull();
  expect(screen.getByText('Newest ComfyUI')).toBeTruthy();
});

test('model choice needs an explicit selection and never auto-picks', async () => {
  fake.llmModels.mockResolvedValue({
    source: 'ollama',
    pull_supported: true,
    models: [{ name: 'qwen3:8b' }],
    roles: [],
  });
  const user = await open();
  await next(user);
  await next(user);
  const select = (await screen.findByLabelText(
    'Text model',
  )) as HTMLSelectElement;
  expect(select.value).toBe('');
  const step = screen.getByRole('navigation', { name: 'Setup steps' });
  expect(within(step).getByText('Not chosen')).toBeTruthy();
  await user.selectOptions(select, 'qwen3:8b');
  expect(within(step).getAllByText('Done').length).toBeGreaterThan(0);
  expect(
    (screen.getByLabelText(/Vision model/) as HTMLSelectElement).value,
  ).toBe('');
});

test('no installed model links to the Models page and can check again', async () => {
  fake.llmModels.mockResolvedValue({
    source: 'ollama',
    pull_supported: true,
    models: [],
    roles: [],
  });
  const user = await open();
  await next(user);
  await next(user);
  const link = await screen.findByRole('link', { name: 'Models page' });
  expect(link.getAttribute('href')).toBe('#/models');
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  await waitFor(() => expect(fake.llmModels).toHaveBeenCalledTimes(2));
});

test('a checkpoints 503 reports ComfyUI unavailable, not an empty list', async () => {
  fake.checkpoints.mockRejectedValue(problem(503, 'ComfyUI is unavailable'));
  const user = await open();
  for (let i = 0; i < 3; i++) await next(user);
  expect(await screen.findByText('ComfyUI is unavailable')).toBeTruthy();
  expect(screen.queryByText(/reports no checkpoints/)).toBeNull();
});

test('zero checkpoints list the locations and steps', async () => {
  fake.checkpoints.mockResolvedValue({
    checkpoints: [],
    locations: [
      {
        environment: 'native',
        directory: '/models/checkpoints',
        steps: ['Copy a `.safetensors` file there.'],
      },
    ],
    refresh_note: 'Refresh ComfyUI afterwards.',
  });
  const user = await open();
  for (let i = 0; i < 3; i++) await next(user);
  expect(await screen.findByText('/models/checkpoints')).toBeTruthy();
  expect(screen.getByText('Refresh ComfyUI afterwards.')).toBeTruthy();
});

async function toReview(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByLabelText(/Use .*1234/));
  await next(user);
  await screen.findByText('ComfyUI answered at http://127.0.0.1:8188.');
  await next(user);
  await user.selectOptions(
    await screen.findByLabelText('Text model'),
    'qwen3:8b',
  );
  await user.selectOptions(screen.getByLabelText(/Vision model/), 'llava:7b');
  await next(user);
  await user.selectOptions(
    await screen.findByLabelText(/Checkpoint for/),
    'sdxl.safetensors',
  );
  await next(user);
  await screen.findByRole('heading', { name: '5. Review and save' });
}

test('review shows every value, saving sends If-Match and then shows the restart keys', async () => {
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot(false, {
      restart_required: ['llm.base_url', 'llm.text_model'],
    }).settings,
  });
  const user = await open();
  await toReview(user);
  const table = screen.getByRole('table', { name: 'Values to save' });
  for (const text of [
    'http://127.0.0.1:8188',
    'http://127.0.0.1:1234/v1',
    'qwen3:8b',
    'llava:7b',
  ])
    expect(
      within(table).getAllByText(
        new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')),
      ),
    ).toBeTruthy();
  expect(screen.getByText('sdxl.safetensors')).toBeTruthy();
  fake.setupStatus.mockResolvedValue(
    status({
      needs_setup: false,
      reasons: [],
      restart_required: ['llm.base_url', 'llm.text_model'],
    }),
  );
  await user.click(screen.getByRole('button', { name: 'Save configuration' }));
  expect(fake.putSettingsWithRevision).toHaveBeenCalledWith(
    {
      'comfyui.base_url': 'http://127.0.0.1:8188',
      'llm.base_url': 'http://127.0.0.1:1234/v1',
      'llm.text_model': 'qwen3:8b',
      'llm.vision_model': 'llava:7b',
    },
    '"rev-1"',
  );
  expect(await screen.findByText('Restart required.')).toBeTruthy();
  expect(screen.getByText('llm.text_model')).toBeTruthy();
  expect(screen.getByText(/restart the GenAImage container/)).toBeTruthy();
});

test('a failed save is reported and never shown as saved', async () => {
  fake.putSettingsWithRevision.mockRejectedValue(
    problem(412, 'Precondition failed'),
  );
  const user = await open();
  await toReview(user);
  await user.click(screen.getByRole('button', { name: 'Save configuration' }));
  expect(await screen.findByText('Precondition failed')).toBeTruthy();
  expect(
    screen.getByRole('button', { name: 'Reload current settings' }),
  ).toBeTruthy();
  expect(screen.queryByText('Configuration saved.')).toBeNull();
});

test('a reload resumes at the restart step from the server state', async () => {
  fake.setupStatus.mockResolvedValue(
    status({
      needs_setup: false,
      reasons: [],
      config_file_exists: true,
      restart_required: ['comfyui.base_url'],
    }),
  );
  render(Wizard);
  expect(
    await screen.findByRole('heading', { name: '6. Apply changes' }),
  ).toBeTruthy();
  expect(screen.getByText('comfyui.base_url')).toBeTruthy();
  expect(screen.getByText('Restart required.')).toBeTruthy();
});

test('dismissal is remembered per set of blocking reasons only as a convenience', () => {
  const current = status() as never;
  expect(isDismissed(current)).toBe(false);
  rememberDismissal(current);
  expect(isDismissed(current)).toBe(true);
  expect(
    isDismissed(
      status({
        reasons: [{ code: 'llm_not_configured', blocking: true, summary: '' }],
      }) as never,
    ),
  ).toBe(false);
});
