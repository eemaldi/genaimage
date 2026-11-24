import {
  render,
  screen,
  waitFor,
  cleanup,
  fireEvent,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import axe from 'axe-core';
import { afterEach, expect, it, vi } from 'vitest';
import { mockNativeDialog } from './test/dialog';
import App from './App.svelte';
import { api } from './lib/api/client';

afterEach(() => {
  cleanup();
  window.location.hash = '';
  localStorage.clear();
  vi.unstubAllGlobals();
  document.title = '';
});

mockNativeDialog();

it('renders the current route and responds to hash changes', async () => {
  window.location.hash = '#/runs/first';
  render(App);
  expect(screen.getByRole('heading', { name: 'Run detail' })).toBeTruthy();
  expect(screen.getByText('Run: first')).toBeTruthy();
  window.location.hash = '#/runs/second?tab=events';
  await waitFor(() => expect(screen.getByText('Run: second')).toBeTruthy());
  window.location.hash = '#/assistant';
  await waitFor(() =>
    expect(
      screen.getByRole('heading', { name: 'Assistant', level: 1 }),
    ).toBeTruthy(),
  );
  window.location.hash = '#/missing';
  await waitFor(() =>
    expect(
      screen.getByRole('heading', { name: 'Page not found' }),
    ).toBeTruthy(),
  );
});

it('identifies the current section, preserves the hash on skip, and moves focus on navigation', async () => {
  window.location.hash = '#/workflows/example';
  const user = userEvent.setup();
  render(App);
  expect(
    screen.getByRole('link', { name: 'Workflows', current: 'location' }),
  ).toBeTruthy();
  expect(screen.getByRole('link', { name: 'Assistant' })).toBeTruthy();
  await user.tab();
  expect(document.activeElement).toBe(
    screen.getByRole('link', { name: 'Skip to main content' }),
  );
  await user.keyboard('{Enter}');
  expect(window.location.hash).toBe('#/workflows/example');
  expect(document.activeElement).toBe(screen.getByRole('main'));
  await user.click(screen.getByRole('link', { name: 'Gallery' }));
  await screen.findByRole('link', { name: 'Gallery', current: 'page' });
  await waitFor(() =>
    expect(document.activeElement).toBe(screen.getByRole('main')),
  );
});

it('exposes a collapsible menu control that closes on Escape and navigation', async () => {
  vi.stubGlobal('matchMedia', () => ({ matches: true }));
  const user = userEvent.setup();
  render(App);
  const toggle = screen.getByRole('button', { name: 'Menu' });
  const nav = screen.getByRole('navigation');
  expect(toggle.getAttribute('aria-controls')).toBe(nav.id);
  expect(toggle.getAttribute('aria-expanded')).toBe('false');
  await user.click(toggle);
  expect(toggle.getAttribute('aria-expanded')).toBe('true');
  screen.getByRole('link', { name: 'Generate' }).focus();
  await user.keyboard('{Escape}');
  expect(document.activeElement).toBe(toggle);
  expect(toggle.getAttribute('aria-expanded')).toBe('false');
  await user.keyboard('{Enter}');
  await user.click(screen.getByRole('link', { name: 'Settings' }));
  await screen.findByRole('heading', { name: 'Settings' });
  expect(toggle.getAttribute('aria-expanded')).toBe('false');
});

it('opens shortcut help and allows character shortcuts to be turned off', async () => {
  const user = userEvent.setup();
  render(App);
  await user.keyboard('gj');
  await screen.findByRole('heading', { name: 'Jobs' });
  await user.keyboard('?');
  expect(
    screen.getByRole('dialog', { name: 'Keyboard shortcuts' }),
  ).toBeTruthy();
  await user.click(
    screen.getByRole('checkbox', { name: 'Enable character-key shortcuts' }),
  );
  await user.click(screen.getByRole('button', { name: 'Close' }));
  await user.keyboard('?gd');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(window.location.hash).toBe('#/jobs');
  expect(localStorage.getItem('genaimage.disable-shortcuts')).toBe('true');
  await user.click(screen.getByRole('button', { name: 'Keyboard shortcuts' }));
  expect(screen.getByRole('dialog')).toBeTruthy();
});

it.each(['hidden', 'unfocusable'])(
  'returns focus to the current route if the menu opener is %s',
  async (state) => {
    window.location.hash = '#/jobs';
    const user = userEvent.setup();
    render(App);
    const toggle = screen.getByRole('button', { name: 'Menu' });
    await user.click(toggle);
    const opener = screen.getByRole('link', { name: 'Generate' });
    opener.focus();
    await user.keyboard('?');
    const help = screen.getByRole('dialog', { name: 'Keyboard shortcuts' });
    window.location.hash = '#/';
    await screen.findByRole('heading', { name: 'Dashboard' });
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(help.contains(document.activeElement)).toBe(true);
    // jsdom does not apply the responsive navigation rule in this suite.
    if (state === 'hidden') screen.getByRole('navigation').hidden = true;
    else opener.removeAttribute('href');
    expect(opener.isConnected).toBe(true);
    await fireEvent(help, new Event('cancel', { cancelable: true }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole('main'));
  },
);

it('has no axe violations in the shell or shortcut help (contrast checked separately)', async () => {
  document.title = 'GenAImage';
  document.documentElement.lang = 'en-GB';
  try {
    const user = userEvent.setup();
    render(App);
    const options = { rules: { 'color-contrast': { enabled: false } } };
    expect((await axe.run(document, options)).violations).toEqual([]);
    await user.click(
      screen.getByRole('button', { name: 'Keyboard shortcuts' }),
    );
    expect((await axe.run(document, options)).violations).toEqual([]);
  } finally {
    document.documentElement.removeAttribute('lang');
  }
});

it('opens and closes the lazy assistant by keyboard with focus return', async () => {
  vi.spyOn(api, 'health').mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local' },
  });
  vi.spyOn(api, 'listAssistantConversations').mockResolvedValue({
    conversations: [],
    next_cursor: null,
  });
  const user = userEvent.setup();
  render(App);
  const toggle = screen.getByRole('button', { name: 'Assistant chat' });
  toggle.focus();
  await user.keyboard('{Enter}');
  const close = await screen.findByRole('button', { name: 'Close assistant' });
  expect(toggle.getAttribute('aria-expanded')).toBe('true');
  expect(document.activeElement).toBe(close);
  await user.keyboard('{Escape}');
  expect(screen.queryByRole('button', { name: 'Close assistant' })).toBeNull();
  expect(document.activeElement).toBe(toggle);
  expect(localStorage.getItem('genaimage.assistant.open')).toBe('false');
});

it('persists the assistant dock side', async () => {
  vi.spyOn(api, 'health').mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local' },
  });
  vi.spyOn(api, 'listAssistantConversations').mockResolvedValue({
    conversations: [],
    next_cursor: null,
  });
  const user = userEvent.setup();
  render(App);
  await user.click(screen.getByRole('button', { name: 'Assistant chat' }));
  await user.click(
    await screen.findByRole('button', { name: 'Dock assistant on the left' }),
  );
  expect(localStorage.getItem('genaimage.assistant.side')).toBe('left');
});
