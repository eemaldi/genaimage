import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { api } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import Packs from './Packs.svelte';

type Pack = components['schemas']['PackSummary'];
type Validation = components['schemas']['PackValidationResponse'];

const pack: Pack = {
  id: 'core',
  version: '1',
  description: 'Core workflows',
  directory: '/packs/core',
  root: '/packs',
  workflow_ids: ['text-to-image'],
  requires: { node_classes: ['KSampler'], models: {} },
  templates: [],
  diagnostics: [],
};
const confirmed: Validation = {
  pack_id: 'core',
  status: 'ok',
  reason: null,
  node_classes: [{ class: 'KSampler', status: 'available' }],
  models: [],
  diagnostics: [],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const settle = () => new Promise((done) => setTimeout(done, 0));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it('drops a check that finishes after the packs were reloaded', async () => {
  vi.spyOn(api, 'listPacks').mockResolvedValue({
    packs: [pack],
    diagnostics: [],
  });
  const stale = deferred<Validation>();
  const fresh = deferred<Validation>();
  vi.spyOn(api, 'validatePack')
    .mockReturnValueOnce(stale.promise)
    .mockReturnValueOnce(fresh.promise);
  const user = userEvent.setup();
  const { rerender } = render(Packs, { generation: 0 });
  await user.click(
    await screen.findByRole('button', { name: 'Check requirements for core' }),
  );
  expect(screen.getByText('Checking requirements…')).toBeTruthy();

  await rerender({ generation: 1 });
  await waitFor(() =>
    expect(screen.queryByText('Checking requirements…')).toBeNull(),
  );
  await user.click(
    await screen.findByRole('button', { name: 'Check requirements for core' }),
  );
  expect(api.validatePack).toHaveBeenCalledTimes(2);

  stale.resolve(confirmed);
  await settle();
  expect(screen.queryByText('Requirements confirmed')).toBeNull();
  expect(screen.getByText('Checking requirements…')).toBeTruthy();

  fresh.resolve({
    ...confirmed,
    status: 'missing',
    node_classes: [{ class: 'KSampler', status: 'missing' }],
  });
  expect(await screen.findByText('Requirements missing')).toBeTruthy();
  expect(screen.queryByText('Requirements confirmed')).toBeNull();
});

it('shows a failed check as an error and never as confirmed', async () => {
  vi.spyOn(api, 'listPacks').mockResolvedValue({
    packs: [pack],
    diagnostics: [],
  });
  vi.spyOn(api, 'validatePack').mockRejectedValue(
    new Error('connection refused'),
  );
  const user = userEvent.setup();
  render(Packs, { generation: 0 });
  await user.click(
    await screen.findByRole('button', { name: 'Check requirements for core' }),
  );
  expect(await screen.findByText('Could not check requirements')).toBeTruthy();
  expect(screen.getByText('connection refused')).toBeTruthy();
  expect(screen.queryByText(/Requirements confirmed/)).toBeNull();
});
