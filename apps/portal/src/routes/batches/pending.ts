import {
  parseBindingJson,
  stringifyBindingJson,
  type SubmitBatchRequest,
} from '../../lib/api/client';

export type PendingBatch = {
  key: string;
  body: Readonly<SubmitBatchRequest>;
  itemCount: number;
};

const prefix = (workflow: string) =>
  `genaimage.batch.pending.v1:${encodeURIComponent(workflow)}:`;

export function savePendingBatch(pending: PendingBatch) {
  sessionStorage.setItem(
    prefix(pending.body.workflow) + pending.key,
    JSON.stringify({
      body: stringifyBindingJson(pending.body),
      itemCount: pending.itemCount,
    }),
  );
}

export function hasPendingBatch(workflow: string): boolean {
  try {
    const start = prefix(workflow);
    for (let i = 0; i < sessionStorage.length; i++)
      if (sessionStorage.key(i)?.startsWith(start)) return true;
  } catch {
    /* Storage is optional for this hint. */
  }
  return false;
}

export function loadPendingBatch(workflow: string): PendingBatch | undefined {
  const start = prefix(workflow);
  for (let i = 0; i < sessionStorage.length; i++) {
    const slot = sessionStorage.key(i);
    if (!slot?.startsWith(start)) continue;
    const { body: raw, itemCount } = JSON.parse(
      sessionStorage.getItem(slot) ?? 'null',
    );
    if (typeof raw !== 'string' || !Number.isSafeInteger(itemCount))
      throw new Error('Could not read the pending batch.');
    const body = parseBindingJson(raw) as SubmitBatchRequest;
    if (body?.workflow !== workflow || stringifyBindingJson(body) !== raw)
      throw new Error('Could not restore the exact pending batch request.');
    return { key: slot.slice(start.length), body, itemCount };
  }
}

export function clearPendingBatch(pending: PendingBatch) {
  sessionStorage.removeItem(prefix(pending.body.workflow) + pending.key);
}
