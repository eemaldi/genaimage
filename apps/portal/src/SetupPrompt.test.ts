import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { mockNativeDialog } from './test/dialog';
import App from './App.svelte';
import { api } from './lib/api/client';

mockNativeDialog();
afterEach(() => {
  cleanup();
  window.location.hash = '';
  localStorage.clear();
  vi.restoreAllMocks();
});

const needsSetup = {
  needs_setup: true,
  reasons: [
    {
      code: 'no_config_file',
      summary: 'No configuration file.',
      blocking: true,
    },
  ],
  config_file: '/c.toml',
  config_file_exists: false,
  endpoints_env_owned: false,
  restart_required: [],
  comfyui: { configured_url: 'http://127.0.0.1:8188', source: 'default' },
  llm: { configured_url: '', source: 'default' },
};

it('opens the wizard automatically when the server needs setup', async () => {
  vi.spyOn(api, 'setupStatus').mockResolvedValue(needsSetup as never);
  vi.spyOn(api, 'getSettingsWithRevision').mockRejectedValue(new Error('x'));
  render(App);
  await screen.findByRole('heading', { name: 'Set up GenAImage' });
  expect(window.location.hash).toBe('#/setup');
});

it('does not reopen after dismissal but the Setup link reopens it', async () => {
  vi.spyOn(api, 'setupStatus').mockResolvedValue(needsSetup as never);
  vi.spyOn(api, 'getSettingsWithRevision').mockResolvedValue({
    etag: '"1"',
    settings: {
      config_file: '/c.toml',
      config_file_exists: false,
      restart_required: [],
      settings: {},
    },
  } as never);
  const user = userEvent.setup();
  render(App);
  await user.click(
    await screen.findByRole('button', { name: 'Dismiss setup' }),
  );
  await screen.findByRole('heading', { name: 'Dashboard', level: 1 });
  cleanup();
  window.location.hash = '#/';
  render(App);
  await screen.findByRole('heading', { name: 'Dashboard', level: 1 });
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(window.location.hash).toBe('#/');
  await user.click(screen.getByRole('link', { name: 'Setup' }));
  await waitFor(() => expect(window.location.hash).toBe('#/setup'));
});

it('does not hijack a deep link to another page', async () => {
  vi.spyOn(api, 'setupStatus').mockResolvedValue(needsSetup as never);
  window.location.hash = '#/jobs';
  render(App);
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(window.location.hash).toBe('#/jobs');
});
