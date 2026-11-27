import {
  parseBindingJson,
  requireExactNumberReads,
  stringifyBindingJson,
  supportsExactNumberWrites,
  type SubmitBatchRequest,
} from '../../lib/api/client';

export type PendingBatch = {
  key: string;
  body: Readonly<SubmitBatchRequest>;
  itemCount: number;
};

export class CorruptPendingBatch extends Error {
  constructor(readonly slot: string) {
    super('Could not restore the exact pending batch request.');
    this.name = 'CorruptPendingBatch';
  }
}

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

function parseStored(workflow: string, text: string | null) {
  try {
    const { body: raw, itemCount } = JSON.parse(text ?? 'null');
    if (typeof raw !== 'string' || !Number.isSafeInteger(itemCount))
      return undefined;
    const body = parseBindingJson(raw) as SubmitBatchRequest;
    if (body?.workflow !== workflow || stringifyBindingJson(body) !== raw)
      return undefined;
    return { body, itemCount };
  } catch {
    return undefined;
  }
}

export function loadPendingBatch(workflow: string): PendingBatch | undefined {
  const start = prefix(workflow);
  for (let i = 0; i < sessionStorage.length; i++) {
    const slot = sessionStorage.key(i);
    if (!slot?.startsWith(start)) continue;
    requireExactNumberReads();
    if (!supportsExactNumberWrites())
      throw new Error('This browser cannot send exact JSON numbers.');
    const stored = parseStored(workflow, sessionStorage.getItem(slot));
    if (!stored) throw new CorruptPendingBatch(slot);
    return { key: slot.slice(start.length), ...stored };
  }
}

export function clearPendingBatch(pending: PendingBatch) {
  sessionStorage.removeItem(prefix(pending.body.workflow) + pending.key);
}

export function discardCorruptPendingBatch(slot: string) {
  sessionStorage.removeItem(slot);
}
