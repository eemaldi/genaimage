import type { api } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';

export type Batch = components['schemas']['BatchResponse'];
const activeStatuses = new Set([
  'created',
  'submitting',
  'unknown_submission',
  'queued',
  'running',
  'cancelling',
]);

export const firstDelay = 1000;
export const maxDelay = 5000;
export const idleDelay = 10_000;
export const maxFailures = 5;

export const hasActiveRuns = (batch: Batch) =>
  batch.items.some(
    (item) => item.run_status && activeStatuses.has(item.run_status),
  );

export type PollerOptions = {
  client: Pick<typeof api, 'getBatch'>;
  id: string;
  onBatch: (batch: Batch) => void;
  onError: (error: unknown) => void;
  /** Whether live run events keep the rows fresh. */
  live: () => boolean;
};

/**
 * Reads a batch, then polls while it is still submitting, with a bounded
 * back-off. An older response never replaces a newer one.
 */
export function createBatchPoller(options: PollerOptions) {
  let sequence = 0;
  let applied = 0;
  let request: AbortController | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let delay = firstDelay;
  let stopped = false;
  let failures = 0;

  function schedule(batch: Batch | undefined) {
    clearTimeout(timer);
    timer = undefined;
    if (stopped) return;
    let wait: number | undefined;
    if (!batch || batch.state === 'submitting') {
      wait = delay;
      delay = Math.min(Math.ceil(delay * 1.5), maxDelay);
    } else if (!options.live() && hasActiveRuns(batch)) wait = idleDelay;
    if (wait !== undefined) timer = setTimeout(() => void refresh(), wait);
  }

  async function refresh() {
    if (stopped) return;
    clearTimeout(timer);
    request?.abort();
    const controller = new AbortController();
    request = controller;
    const mine = ++sequence;
    try {
      const batch = await options.client.getBatch(options.id, {
        signal: controller.signal,
      });
      if (stopped || controller.signal.aborted || mine < applied) return;
      applied = mine;
      failures = 0;
      options.onBatch(batch);
      if (batch.state !== 'submitting') delay = firstDelay;
      schedule(batch);
    } catch (error) {
      if (stopped || controller.signal.aborted) return;
      options.onError(error);
      if (++failures < maxFailures) schedule(undefined);
    } finally {
      if (request === controller) request = undefined;
    }
  }

  return {
    refresh,
    /** Take a batch from another response, such as a resume, unless it is older. */
    accept(batch: Batch) {
      applied = ++sequence;
      request?.abort();
      options.onBatch(batch);
      schedule(batch);
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
      request?.abort();
    },
  };
}
