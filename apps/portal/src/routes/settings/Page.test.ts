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

const fake = vi.hoisted(() => ({
  getSettings: vi.fn<typeof api.getSettings>(),
  putSettings: vi.fn<typeof api.putSettings>(),
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
  fake.getSettings.mockResolvedValue(settings);
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
  expect(fake.putSettings).not.toHaveBeenCalled();
});
test('maps server errors to fields and keeps the form dirty', async () => {
  await start();
  fake.putSettings.mockRejectedValue(serverProblem());
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
test('tests unsaved values without saving and reports results and problems', async () => {
  await start();
  fake.testSettings
    .mockResolvedValueOnce({ comfyui: 'ok', llm: 'unavailable' })
    .mockRejectedValueOnce(serverProblem());
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234/v1' },
  });
  await userEvent.click(
    screen.getByRole('button', { name: 'Test ComfyUI connection' }),
  );
  expect(fake.testSettings).toHaveBeenCalledWith({
    'llm.base_url': 'http://localhost:1234/v1',
  });
  expect(await screen.findByText('ComfyUI: ok')).toBeTruthy();
  expect(screen.getByText('LLM: unavailable')).toBeTruthy();
  expect(fake.putSettings).not.toHaveBeenCalled();
  await userEvent.click(
    screen.getByRole('button', { name: 'Test LLM connection' }),
  );
  expect(await screen.findByText('Correct the listed settings.')).toBeTruthy();
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
  fake.putSettings.mockResolvedValue(saved);
  fake.getSettings.mockResolvedValue(saved);
  await fireEvent.input(screen.getByLabelText('Server port'), {
    target: { value: '3001' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  await waitFor(() => expect(fake.getSettings).toHaveBeenCalledTimes(2));
  expect(fake.putSettings).toHaveBeenCalledWith({ 'server.port': 3001 });
  expect(await screen.findByText(/Restart required:/)).toBeTruthy();
  expect(screen.getByText(/Active value until restart: 3000/)).toBeTruthy();
  expect(screen.getByText(/No unsaved changes/)).toBeTruthy();
});
test('guards hash navigation and beforeunload while dirty', async () => {
  await start();
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  const confirm = vi
    .spyOn(window, 'confirm')
    .mockReturnValueOnce(false)
    .mockReturnValueOnce(true);
  const laterListener = vi.fn();
  window.addEventListener('hashchange', laterListener);
  window.history.replaceState(null, '', '#/jobs');
  window.dispatchEvent(new HashChangeEvent('hashchange'));
  expect(window.location.hash).toBe('#/settings');
  expect(laterListener).not.toHaveBeenCalled();
  const unload = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(unload);
  expect(unload.defaultPrevented).toBe(true);
  window.history.replaceState(null, '', '#/jobs');
  window.dispatchEvent(new HashChangeEvent('hashchange'));
  expect(confirm).toHaveBeenCalledTimes(2);
  expect(laterListener).toHaveBeenCalledTimes(1);
  window.removeEventListener('hashchange', laterListener);
});

test('guard prevents the shell routing away until discard is confirmed', async () => {
  fake.getSettings.mockResolvedValue(fixture());
  vi.spyOn(window, 'confirm')
    .mockReturnValueOnce(false)
    .mockReturnValueOnce(true);
  render(App);
  await screen.findByLabelText('LLM URL');
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(
    screen.getByRole('link', { name: 'Assistant Planned' }),
  );
  await waitFor(() => expect(window.location.hash).toBe('#/settings'));
  expect(screen.getByLabelText('LLM URL')).toBeTruthy();
  await userEvent.click(
    screen.getByRole('link', { name: 'Assistant Planned' }),
  );
  await screen.findByText('The assistant is planned and not yet available.');
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
  fake.putSettings.mockResolvedValue(saved);
  fake.getSettings.mockResolvedValue(saved);
  await fireEvent.input(screen.getByLabelText('LLM URL'), {
    target: { value: 'http://localhost:1234' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  expect(
    await screen.findByText('All saved settings are active without a restart.'),
  ).toBeTruthy();
});
