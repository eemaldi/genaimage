import type { Batch } from './poller';

type Item = Batch['items'][number];
export const item = (index: number, extra: Partial<Item> = {}): Item => ({
  index,
  parameters: { seed: index },
  sweep_values: { seed: index },
  run_id: null,
  submission: 'pending',
  error: null,
  run_status: null,
  ...extra,
});

export const batch = (extra: Partial<Batch> = {}): Batch => {
  const items = extra.items ?? [item(0), item(1)];
  return {
    id: 'batch-1',
    created_at: '2026-11-27T10:00:00Z',
    workflow: 'portrait',
    workflow_revision: 'rev-1',
    item_count: items.length,
    state: 'submitting',
    counts: {
      pending: items.filter((i) => i.submission === 'pending').length,
      submitted: items.filter((i) => i.submission === 'submitted').length,
      failed: items.filter((i) => i.submission === 'failed').length,
      not_submitted: items.filter((i) => i.submission === 'not_submitted')
        .length,
      outcome_unknown: items.filter((i) => i.submission === 'outcome_unknown')
        .length,
    },
    preset: null,
    parameters: {},
    sweep: { seeds: { parameter: 'seed', values: items.map((i) => i.index) } },
    max_items: items.length,
    items,
    ...extra,
  };
};
