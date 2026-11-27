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
const stepItem = (label: string) =>
  within(screen.getByRole('navigation', { name: 'Setup steps' }))
    .getByText(new RegExp(label))
    .closest('li')!;
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
  expect(stepItem('LLM and models').textContent).toContain('Done');
  expect(stepItem('Detect services').textContent).toContain('Ran');
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
  expect(screen.queryByText(/no pending restart/)).toBeNull();
});

const save = (user: ReturnType<typeof userEvent.setup>) =>
  user.click(screen.getByRole('button', { name: /^Save/ }));
const back = async (
  user: ReturnType<typeof userEvent.setup>,
  times: number,
) => {
  for (let i = 0; i < times; i++)
    await user.click(screen.getByRole('button', { name: 'Back' }));
};

test('a failed status refresh never claims there is no pending restart', async () => {
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot(false, { restart_required: [] }).settings,
  });
  const user = await open();
  await toReview(user);
  fake.setupStatus.mockRejectedValue(new Error('offline'));
  await save(user);
  expect(
    await screen.findByText(/could not confirm the restart state/),
  ).toBeTruthy();
  expect(screen.queryByText(/no pending restart/)).toBeNull();
  expect(stepItem('Apply changes').textContent).toContain('unconfirmed');
  fake.setupStatus.mockResolvedValue(
    status({ needs_setup: false, reasons: [] }),
  );
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  expect(await screen.findByText(/reports no pending restart/)).toBeTruthy();
  expect(stepItem('Apply changes').textContent).toContain('Done');
});

test('a failed refresh falls back to the restart keys of the save response', async () => {
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot(false, { restart_required: ['llm.text_model'] })
      .settings,
  });
  const user = await open();
  await toReview(user);
  fake.setupStatus.mockRejectedValue(new Error('offline'));
  await save(user);
  expect(await screen.findByText('Restart required.')).toBeTruthy();
  expect(screen.getByText('llm.text_model')).toBeTruthy();
  expect(screen.queryByText(/no pending restart/)).toBeNull();
});

test('Saved and Done are invalidated when a choice changes after saving', async () => {
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot(false, { restart_required: ['llm.base_url'] }).settings,
  });
  const user = await open();
  await toReview(user);
  fake.setupStatus.mockResolvedValue(
    status({ restart_required: ['llm.base_url'] }),
  );
  await save(user);
  await screen.findByText('Restart required.');
  expect(stepItem('Review and save').textContent).toContain('Saved');
  await back(user, 5);
  await user.click(screen.getAllByLabelText(/Keep the configured endpoint/)[1]);
  expect(stepItem('Review and save').textContent).toContain(
    'Changed since saved',
  );
  expect(stepItem('LLM and models').textContent).not.toContain('Done');
});

test('the review lists unverified items and the checkpoint is marked running-server only', async () => {
  fake.testSettings.mockResolvedValueOnce({
    comfyui: 'failed',
    comfyui_reason: 'refused',
  });
  const user = await open();
  for (let i = 0; i < 3; i++) await next(user);
  await user.selectOptions(
    await screen.findByLabelText(/Checkpoint for/),
    'sdxl.safetensors',
  );
  await next(user);
  expect(
    await screen.findByText('Not verified for these choices:'),
  ).toBeTruthy();
  expect(screen.getByText(/ComfyUI at .* was not tested/)).toBeTruthy();
});

test('Compose saves no environment-owned keys', async () => {
  fake.setupStatus.mockResolvedValue(
    status({
      endpoints_env_owned: true,
      comfyui: { configured_url: 'http://comfyui:8188', source: 'env' },
      llm: { configured_url: 'http://llm:11434/v1', source: 'env' },
    }),
  );
  fake.getSettingsWithRevision.mockResolvedValue(snapshot(true));
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot(true).settings,
  });
  const user = await open();
  await next(user);
  await next(user);
  await user.selectOptions(
    await screen.findByLabelText('Text model'),
    'qwen3:8b',
  );
  await next(user);
  await next(user);
  await screen.findByRole('heading', { name: '5. Review and save' });
  await save(user);
  expect(fake.putSettingsWithRevision).toHaveBeenCalledWith(
    { 'llm.text_model': 'qwen3:8b' },
    '"rev-1"',
  );
});

test('skipping the LLM saves no LLM values', async () => {
  fake.putSettingsWithRevision.mockResolvedValue({
    etag: '"rev-2"',
    settings: snapshot().settings,
  });
  const user = await open();
  await next(user);
  await next(user);
  await user.click(
    await screen.findByRole('button', { name: 'Skip LLM setup' }),
  );
  expect(stepItem('LLM and models').textContent).toContain('Skipped');
  await next(user);
  await next(user);
  expect(screen.getByText(/LLM setup was skipped/)).toBeTruthy();
  await save(user);
  expect(fake.putSettingsWithRevision).toHaveBeenCalledWith(
    { 'comfyui.base_url': 'http://127.0.0.1:8188' },
    '"rev-1"',
  );
});

test('a 412 on the ComfyUI test is reported and the step stays unverified', async () => {
  fake.testSettings.mockRejectedValue(
    new ApiProblemError({ title: 'Precondition failed', status: 412 } as never),
  );
  const user = await open();
  await next(user);
  expect(await screen.findByText('Precondition failed')).toBeTruthy();
  expect(screen.getByText(/Reload this page/)).toBeTruthy();
  expect(stepItem('Test ComfyUI').textContent).toContain('Not verified');
});

test('an unknown save outcome is not reported as nothing saved', async () => {
  fake.putSettingsWithRevision.mockRejectedValue(new Error('connection reset'));
  const user = await open();
  await toReview(user);
  await save(user);
  expect(
    await screen.findByText('The outcome of the save is unknown'),
  ).toBeTruthy();
  expect(screen.queryByText(/Nothing was saved/)).toBeNull();
  expect(
    screen.getByRole('button', { name: 'Reload current settings' }),
  ).toBeTruthy();
  expect(stepItem('Review and save').textContent).toContain('Not saved');
});

test('dismissal storage that throws is tolerated', () => {
  const broken = vi
    .spyOn(Storage.prototype, 'getItem')
    .mockImplementation(() => {
      throw new Error('blocked');
    });
  const write = vi
    .spyOn(Storage.prototype, 'setItem')
    .mockImplementation(() => {
      throw new Error('blocked');
    });
  expect(isDismissed(status() as never)).toBe(false);
  expect(() => rememberDismissal(status() as never)).not.toThrow();
  broken.mockRestore();
  write.mockRestore();
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
