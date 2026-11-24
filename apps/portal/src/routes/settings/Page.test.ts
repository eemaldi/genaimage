import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, type api } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import Page from './Page.svelte';
import App from '../../App.svelte';
import { formFrom, validate } from './form';

const fake = vi.hoisted(() => ({
  getSettingsWithRevision: vi.fn<typeof api.getSettingsWithRevision>(),
  putSettingsWithRevision: vi.fn<typeof api.putSettingsWithRevision>(),
  testSettings: vi.fn<typeof api.testSettings>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));
type Settings = components['schemas']['SettingsResponse'];
function fixture(): Settings {
  const values: Record<string, unknown> = {
    'comfyui.base_url': 'http://127.0.0.1:8188',
    'comfyui.allowed_hosts': [],
    'comfyui.request_timeout_seconds': 60,
    'comfyui.websocket_reconnect_seconds': 2,
    'llm.base_url': '',
    'llm.model': 'local',
    'llm.allowed_hosts': [],
    'paths.state_database': '/state.sqlite3',
    'paths.workflows': '/workflows',
    'paths.runs': '/runs',
    'paths.uploads': '/uploads',
    'server.host': '127.0.0.1',
    'server.port': 3000,
    'server.max_upload_bytes': 1024,
    claim_legacy_state: false,
  };
  const settings: Settings['settings'] = Object.fromEntries(
    Object.entries(values).map(([key, value]) => [
      key,
      {
        value,
        source: 'default' as const,
        read_only: key === 'claim_legacy_state',
      },
    ]),
  );
  settings['llm.model'] = {
    value: 'local',
    source: 'env',
    variable: 'GENAIMAGE_LLM_MODEL',
    read_only: true,
  };
  settings['paths.runs'] = {
    value: '/runs',
    source: 'file',
    file: '/config.toml',
    read_only: false,
  };
  return { settings, config_file: '/config.toml', restart_required: [] };
}
async function start(settings = fixture()) {
  fake.getSettingsWithRevision.mockResolvedValue({ settings, etag: '"rev1"' });
  render(Page, { params: {}, query: new URLSearchParams() });
  await screen.findByLabelText('ComfyUI URL');
}
function serverProblem() {
  return new ApiProblemError({
    type: 'about:blank',
    title: 'Invalid settings',
    status: 422,
    detail: 'One or more settings are invalid.',
    recovery: 'Correct the listed settings.',
    errors: [
      { key: 'paths.state_database', reason: 'Choose a writable database.' },
    ],
  });
}
beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, '', '#/settings');
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.history.replaceState(null, '', '/');
});

test('shows sources and locks environment overrides', async () => {
  await start();
  expect(screen.getAllByText(/Default value/).length).toBeGreaterThan(0);
  expect(screen.getByText(/Config file: \/config.toml/)).toBeTruthy();
  expect(
    screen.getByText(
      /Environment variable GENAIMAGE_LLM_MODEL.*change or remove/,
    ),
  ).toBeTruthy();
  expect(screen.getByLabelText('LLM model').hasAttribute('readonly')).toBe(
    true,
  );
});
test('blocks invalid URLs, numbers and paths and saves only valid edits', async () => {
  await start();
  const button = screen.getByRole('button', {
    name: 'Save settings',
  }) as HTMLButtonElement;
  await fireEvent.input(screen.getByLabelText('ComfyUI URL'), {
    target: { value: 'https://example.com' },
  });
  expect(screen.getByText(/Use a loopback URL here/)).toBeTruthy();
  expect(button.disabled).toBe(true);
  await fireEvent.input(screen.getByLabelText('ComfyUI URL'), {
    target: { value: 'http://localhost:8188' },
  });
  await fireEvent.input(screen.getByLabelText('Server port'), {
    target: { value: '65536' },
  });
  expect(button.disabled).toBe(true);
  await fireEvent.input(screen.getByLabelText('Server port'), {
    target: { value: '3000' },
  });
  await fireEvent.input(screen.getByLabelText('Runs directory'), {
    target: { value: '' },
  });
  expect(button.disabled).toBe(true);
  expect(fake.putSettingsWithRevision).not.toHaveBeenCalled();
});
test('maps server errors to fields and keeps the form dirty', async () => {
  await start();
  fake.putSettingsWithRevision.mockRejectedValue(serverProblem());
  await fireEvent.input(screen.getByLabelText('State database'), {
    target: { value: '/new.sqlite3' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  const input = screen.getByLabelText('State database');
  await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
  expect(
    screen.getAllByText(/Choose a writable database/).length,
  ).toBeGreaterThan(0);
  expect(screen.getByText(/Unsaved changes/)).toBeTruthy();
  expect(
    (screen.getByRole('button', { name: 'Save settings' }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
});
test('tests each service separately and probe errors do not block saving', async () => {
  await start();
  fake.testSettings
    .mockResolvedValueOnce({ comfyui: 'ok', llm: 'not_configured' })
    .mockRejectedValueOnce(serverProblem());
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234/v1' },
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Test ComfyUI connection' }),
  );
  expect(fake.testSettings).toHaveBeenCalledWith(
    {},
    { service: 'comfyui', ifMatch: '"rev1"' },
  );
  expect(await screen.findByText('ComfyUI: ok')).toBeTruthy();
  expect(screen.queryByText('LLM: not configured')).toBeNull();
  await userEvent.click(
    screen.getByRole('button', { name: 'Test LLM connection' }),
  );
  expect(fake.testSettings).toHaveBeenCalledWith(
    { 'llm.base_url': 'http://localhost:1234/v1' },
    { service: 'llm', ifMatch: '"rev1"' },
  );
  expect(await screen.findByText('Correct the listed settings.')).toBeTruthy();
  expect(
    (screen.getByRole('button', { name: 'Save settings' }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
  expect(fake.putSettingsWithRevision).not.toHaveBeenCalled();
});
test('saves then refetches and reports staged restart values', async () => {
  const initial = fixture();
  await start(initial);
  const saved = fixture();
  saved.settings['server.port'] = {
    value: 3001,
    source: 'file',
    file: '/config.toml',
    read_only: false,
    active_value: 3000,
  };
  saved.restart_required = ['server.port'];
  fake.putSettingsWithRevision.mockResolvedValue({
    settings: saved,
    etag: '"rev2"',
  });
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: saved,
    etag: '"rev2"',
  });
  await fireEvent.input(screen.getByLabelText('Server port'), {
    target: { value: '3001' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  await waitFor(() =>
    expect(fake.getSettingsWithRevision).toHaveBeenCalledTimes(2),
  );
  expect(fake.putSettingsWithRevision).toHaveBeenCalledWith(
    { 'server.port': 3001 },
    '"rev1"',
  );
  expect(await screen.findByText(/Restart required:/)).toBeTruthy();
  expect(screen.getByText(/Active value until restart: 3000/)).toBeTruthy();
  expect(screen.getByText(/No unsaved changes/)).toBeTruthy();
});
test('cancelling Back preserves jobs for the next Back traversal', async () => {
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: fixture(),
    etag: '"rev1"',
  });
  vi.spyOn(window, 'confirm')
    .mockReturnValueOnce(false)
    .mockReturnValueOnce(true);
  window.history.replaceState(null, '', '#/');
  render(App);
  await userEvent.click(screen.getByRole('link', { name: 'Jobs' }));
  await screen.findByRole('heading', { name: 'Jobs' });
  await userEvent.click(screen.getByRole('link', { name: 'Settings' }));
  await screen.findByLabelText('LLM URL');
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  history.back();
  await waitFor(() => expect(window.confirm).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(location.hash).toBe('#/settings'));
  expect(screen.getByLabelText('LLM URL')).toBeTruthy();
  history.back();
  await screen.findByRole('heading', { name: 'Jobs' });
  expect(location.hash).toBe('#/jobs');
});

test('cancelling Back to an unmarked entry restores the settings URL and page', async () => {
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: fixture(),
    etag: '"rev1"',
  });
  vi.spyOn(window, 'confirm')
    .mockReturnValueOnce(false)
    .mockReturnValueOnce(true);
  history.pushState(null, '', '#/jobs');
  history.pushState(null, '', '#/settings?tab=paths');
  render(App);
  await screen.findByLabelText('LLM URL');
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  history.back();
  await waitFor(() => expect(window.confirm).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(location.hash).toBe('#/settings?tab=paths'));
  expect(screen.getByRole('heading', { name: 'Settings' })).toBeTruthy();
  expect((screen.getByLabelText('LLM URL') as HTMLInputElement).value).toBe(
    'http://localhost:1234',
  );
  history.back();
  await screen.findByRole('heading', { name: 'Jobs' });
  expect(location.hash).toBe('#/jobs');
});

test('probe revision conflict preserves edits and offers explicit review', async () => {
  await start();
  fake.testSettings.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Precondition failed',
      status: 412,
      detail: 'Settings changed since they were loaded.',
      recovery: 'Reload settings and review your changes.',
    }),
  );
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Test ComfyUI connection' }),
  );
  expect(fake.testSettings).toHaveBeenCalledWith(
    {},
    { service: 'comfyui', ifMatch: '"rev1"' },
  );
  expect(
    await screen.findByText('Settings changed since they were loaded.'),
  ).toBeTruthy();
  expect(
    (screen.getByRole('button', { name: 'Save settings' }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  const updated = fixture();
  updated.settings['llm.base_url'] = {
    value: 'http://localhost:5678',
    source: 'default',
    read_only: false,
  };
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: updated,
    etag: '"rev2"',
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Reload settings for review' }),
  );
  expect(
    await screen.findByText(
      /current http:\/\/localhost:5678, your change http:\/\/localhost:1234/,
    ),
  ).toBeTruthy();
  await userEvent.click(
    screen.getByRole('button', { name: 'Re-apply my changes' }),
  );
  expect((screen.getByLabelText('LLM URL') as HTMLInputElement).value).toBe(
    'http://localhost:1234',
  );
  fake.testSettings.mockResolvedValueOnce({
    comfyui: 'ok',
    llm: 'not_configured',
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Test ComfyUI connection' }),
  );
  expect(fake.testSettings).toHaveBeenLastCalledWith(
    {},
    { service: 'comfyui', ifMatch: '"rev2"' },
  );
});

test('blocks navigation while saving', async () => {
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: fixture(),
    etag: '"rev1"',
  });
  let finish!: (
    value: Awaited<ReturnType<typeof api.putSettingsWithRevision>>,
  ) => void;
  fake.putSettingsWithRevision.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  const alert = vi.spyOn(window, 'alert').mockImplementation(() => {});
  render(App);
  await screen.findByLabelText('LLM URL');
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  await userEvent.click(
    screen.getByRole('link', { name: 'Assistant Planned' }),
  );
  await waitFor(() =>
    expect(alert).toHaveBeenCalledWith(
      expect.stringContaining('save is in progress'),
    ),
  );
  await waitFor(() => expect(location.hash).toBe('#/settings'));
  finish({ settings: fixture(), etag: '"rev2"' });
  await waitFor(() =>
    expect(fake.getSettingsWithRevision).toHaveBeenCalledTimes(2),
  );
});

test('conflict reload keeps edits for explicit review and re-application', async () => {
  await start();
  fake.putSettingsWithRevision.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Precondition failed',
      status: 412,
      detail: 'Settings changed.',
      recovery: 'Reload settings.',
    }),
  );
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  const updated = fixture();
  updated.settings['llm.base_url'] = {
    value: 'http://localhost:5678',
    source: 'default',
    read_only: false,
  };
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: updated,
    etag: '"rev2"',
  });
  await userEvent.click(
    await screen.findByRole('button', { name: 'Reload settings for review' }),
  );
  expect(
    await screen.findByText(
      /current http:\/\/localhost:5678, your change http:\/\/localhost:1234/,
    ),
  ).toBeTruthy();
  expect((screen.getByLabelText('LLM URL') as HTMLInputElement).value).toBe(
    'http://localhost:5678',
  );
  await userEvent.click(
    screen.getByRole('button', { name: 'Re-apply my changes' }),
  );
  expect((screen.getByLabelText('LLM URL') as HTMLInputElement).value).toBe(
    'http://localhost:1234',
  );
});

test('a failed conflict reload keeps the draft and allows retry', async () => {
  await start();
  fake.putSettingsWithRevision.mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Precondition failed',
      status: 412,
      detail: 'Settings changed.',
      recovery: 'Reload settings.',
    }),
  );
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  fake.getSettingsWithRevision.mockRejectedValueOnce(new Error('offline'));
  await userEvent.click(
    await screen.findByRole('button', { name: 'Reload settings for review' }),
  );
  expect(await screen.findByText('offline')).toBeTruthy();
  expect(
    screen.getByRole('button', { name: 'Reload settings for review' }),
  ).toBeTruthy();
  expect(
    screen.queryByRole('button', { name: 'Re-apply my changes' }),
  ).toBeNull();
  expect((screen.getByLabelText('LLM URL') as HTMLInputElement).value).toBe(
    'http://localhost:1234',
  );
});

test('an invalid other service does not prevent a connection test', async () => {
  await start();
  fake.testSettings.mockResolvedValue({ comfyui: 'ok', llm: 'not_configured' });
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'https://example.com' },
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Test ComfyUI connection' }),
  );
  expect(fake.testSettings).toHaveBeenCalledWith(
    {},
    { service: 'comfyui', ifMatch: '"rev1"' },
  );
  expect(await screen.findByText('ComfyUI: ok')).toBeTruthy();
});

test('validation follows core endpoint policy vectors and proposed allow-lists', () => {
  const settings = fixture();
  const form = formFrom(settings);
  for (const host of [
    'localhost',
    '127.0.0.5',
    '::1',
    '0:0:0:0:0:0:0:1',
    '::ffff:127.0.0.1',
  ]) {
    form['server.host'] = host;
    expect(validate(form, settings)['server.host'], host).toBeUndefined();
  }
  form['server.host'] = '127.0.0.1';
  for (const host of [
    'localhost',
    '127.0.0.5',
    '[::1]',
    '[::ffff:127.0.0.1]',
  ]) {
    form['llm.base_url'] = `http://${host}:8188`;
    expect(validate(form, settings)['llm.base_url'], host).toBeUndefined();
  }
  form['llm.allowed_hosts'] = '["bad:face"]';
  expect(validate(form, settings)['llm.allowed_hosts']).toBeTruthy();
  settings.settings['llm.base_url'] = {
    value: 'http://comfyui:8188',
    source: 'file',
    file: '/config',
    read_only: false,
  };
  settings.settings['llm.allowed_hosts'] = {
    value: ['comfyui'],
    source: 'file',
    file: '/config',
    read_only: false,
  };
  const proposed = formFrom(settings);
  expect(validate(proposed, settings)['llm.base_url']).toBeUndefined();
  proposed['llm.allowed_hosts'] = '[]';
  expect(validate(proposed, settings)['llm.base_url']).toMatch(/allow-list/);
});

test('reports live application after a successful refresh', async () => {
  await start();
  const saved = fixture();
  saved.settings['llm.base_url'] = {
    value: 'http://localhost:1234',
    source: 'file',
    file: '/config.toml',
    read_only: false,
  };
  fake.putSettingsWithRevision.mockResolvedValue({
    settings: saved,
    etag: '"rev2"',
  });
  fake.getSettingsWithRevision.mockResolvedValue({
    settings: saved,
    etag: '"rev2"',
  });
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  expect(
    await screen.findByText('All saved settings are active without a restart.'),
  ).toBeTruthy();
});
