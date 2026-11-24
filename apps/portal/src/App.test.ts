import { render, screen, waitFor, cleanup } from '@testing-library/svelte';
import { afterEach, expect, it } from 'vitest';
import App from './App.svelte';

afterEach(() => {
  cleanup();
  window.location.hash = '';
});

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
      screen.getByText('The assistant is planned and not yet available.'),
    ).toBeTruthy(),
  );
  window.location.hash = '#/missing';
  await waitFor(() =>
    expect(
      screen.getByRole('heading', { name: 'Page not found' }),
    ).toBeTruthy(),
  );
});
