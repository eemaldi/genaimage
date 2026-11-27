import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { batch, item } from './fixtures';
import {
  createBatchPoller,
  firstDelay,
  maxDelay,
  maxFailures,
  type Batch,
} from './poller';

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function setup(live = () => true) {
  const getBatch = vi.fn<(id: string) => Promise<Batch>>();
  const seen: Batch[] = [];
  const errors: unknown[] = [];
  const poller = createBatchPoller({
    client: { getBatch } as never,
    id: 'batch-1',
    onBatch: (next) => seen.push(next),
    onError: (error) => errors.push(error),
    live,
  });
  return { getBatch, seen, errors, poller };
}

test('it polls with a bounded back-off while submitting and stops once it is not', async () => {
  const { getBatch, poller, seen } = setup();
  getBatch.mockResolvedValue(batch({ state: 'submitting' }));
  await poller.refresh();
  expect(getBatch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(firstDelay);
  expect(getBatch).toHaveBeenCalledTimes(2);
  for (let i = 0; i < 6; i++) await vi.advanceTimersByTimeAsync(maxDelay);
  expect(getBatch.mock.calls.length).toBeGreaterThanOrEqual(7);
  getBatch.mockResolvedValue(batch({ state: 'submitted' }));
  await vi.advanceTimersByTimeAsync(maxDelay);
  const done = getBatch.mock.calls.length;
  expect(seen.at(-1)?.state).toBe('submitted');
  await vi.advanceTimersByTimeAsync(60_000);
  expect(getBatch).toHaveBeenCalledTimes(done);
  poller.stop();
});

test('an older response that arrives late never replaces a newer one', async () => {
  const { getBatch, poller, seen } = setup();
  let releaseOld!: (value: Batch) => void;
  getBatch.mockReturnValueOnce(
    new Promise((resolve) => (releaseOld = resolve)),
  );
  const old = poller.refresh();
  getBatch.mockResolvedValueOnce(batch({ state: 'submitted' }));
  await poller.refresh();
  releaseOld(batch({ state: 'submitting' }));
  await old;
  expect(seen.map((b) => b.state)).toEqual(['submitted']);
  poller.stop();
});

test('a batch handed over by a resume wins over an in-flight read', async () => {
  const { getBatch, poller, seen } = setup();
  let release!: (value: Batch) => void;
  getBatch.mockReturnValueOnce(new Promise((resolve) => (release = resolve)));
  const pending = poller.refresh();
  poller.accept(batch({ state: 'submitted' }));
  release(batch({ state: 'interrupted' }));
  await pending;
  expect(seen.map((b) => b.state)).toEqual(['submitted']);
  poller.stop();
});

test('it stops after repeated failures and when stopped', async () => {
  const { getBatch, poller, errors } = setup();
  getBatch.mockRejectedValue(new Error('down'));
  await poller.refresh();
  for (let i = 0; i < 10; i++) await vi.advanceTimersByTimeAsync(maxDelay);
  expect(getBatch).toHaveBeenCalledTimes(maxFailures);
  expect(errors).toHaveLength(maxFailures);
  poller.stop();
});

test('without live events it keeps a slow poll only while runs are active', async () => {
  const { getBatch, poller } = setup(() => false);
  getBatch.mockResolvedValue(
    batch({
      state: 'submitted',
      items: [
        item(0, {
          submission: 'submitted',
          run_id: 'r',
          run_status: 'running',
        }),
      ],
    }),
  );
  await poller.refresh();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(getBatch).toHaveBeenCalledTimes(2);
  poller.stop();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(getBatch).toHaveBeenCalledTimes(2);
});
