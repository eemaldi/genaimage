import {
  parseBindingJson,
  stringifyBindingJson,
  type SubmitRunRequest,
} from '../../lib/api/client';

export type PendingSubmission = {
  key: string;
  body: Readonly<SubmitRunRequest>;
  version: string;
  runId?: string;
};

function prefix(workflow: string) {
  return `genaimage.generate.pending.v1:${encodeURIComponent(workflow)}:`;
}

export function savePending(pending: PendingSubmission) {
  const body = stringifyBindingJson(pending.body);
  sessionStorage.setItem(
    prefix(pending.body.workflow) + pending.key,
    JSON.stringify({ body, version: pending.version, runId: pending.runId }),
  );
}

export function loadPending(workflow: string): PendingSubmission | undefined {
  const start = prefix(workflow);
  for (let i = 0; i < sessionStorage.length; i++) {
    const slot = sessionStorage.key(i);
    if (!slot?.startsWith(start)) continue;
    const {
      body: raw,
      version,
      runId,
    } = JSON.parse(sessionStorage.getItem(slot) ?? 'null');
    if (typeof raw !== 'string' || typeof version !== 'string')
      throw new Error('Could not read the pending run.');
    const body = parseBindingJson(raw) as SubmitRunRequest;
    if (body?.workflow !== workflow || stringifyBindingJson(body) !== raw)
      throw new Error('Could not restore the exact pending request.');
    return {
      key: slot.slice(start.length),
      body,
      version,
      runId: typeof runId === 'string' ? runId : undefined,
    };
  }
}

export function clearPending(pending: PendingSubmission) {
  sessionStorage.removeItem(prefix(pending.body.workflow) + pending.key);
}
