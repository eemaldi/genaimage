import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, expect, test, vi } from 'vitest';
import type { api } from '../../lib/api/client';
import BatchList from './BatchList.svelte';

const fake = vi.hoisted(() => ({
  listBatches: vi.fn<typeof api.listBatches>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const summary = {
  id: 'batch-1',
  created_at: '2026-11-27T10:00:00Z',
  workflow: 'portrait',
  workflow_revision: 'r',
  item_count: 4,
  state: 'submitted' as const,
  counts: {
    pending: 0,
    submitted: 4,
    failed: 0,
    not_submitted: 0,
    outcome_unknown: 0,
  },
};

test('batches are listed one bounded page at a time with a cursor link', async () => {
  fake.listBatches.mockResolvedValue({
    batches: [summary],
    next_cursor: 'next-1',
  });
  render(BatchList, {
    query: new URLSearchParams('view=batches&workflow=portrait'),
  });
  expect(await screen.findByRole('link', { name: 'batch-1' })).toBeTruthy();
  expect(fake.listBatches).toHaveBeenCalledWith(
    { workflow: 'portrait', cursor: undefined, limit: 25 },
    expect.anything(),
  );
  expect(
    screen.getByRole('link', { name: 'Older batches' }).getAttribute('href'),
  ).toBe('#/jobs?view=batches&workflow=portrait&cursor=next-1');
  expect(screen.getByText('All runs sent')).toBeTruthy();
  expect(screen.getByRole('link', { name: 'Runs' }).getAttribute('href')).toBe(
    '#/jobs?batch=batch-1',
  );
});
