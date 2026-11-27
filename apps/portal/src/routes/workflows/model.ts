import type { components } from '../../lib/api/schema';

type PackValidation = components['schemas']['PackValidationResponse'];
type Verdict = 'ok' | 'missing' | 'unknown';

export type ValidationView = {
  verdict: Verdict;
  title: string;
  headline: string;
  reason: string | null;
  nodes: { name: string; label: string }[];
  models: {
    folder: string;
    name: string;
    label: string;
    loader: string | null;
    reason: string | null;
  }[];
  diagnostics: string[];
};

const PACK_PREFIX = 'pack:';
const KNOWN_STATUSES = ['ok', 'missing', 'unknown', 'unavailable'];

export function sourceLabel(source: string | null | undefined): string {
  if (!source) return 'Unknown source';
  if (source === 'user') return 'Your workflows';
  if (source.startsWith(PACK_PREFIX))
    return source.slice(PACK_PREFIX.length) || 'Unknown source';
  return source;
}

export function compareSources(left: string, right: string): number {
  if (left === right) return 0;
  if (left === 'user') return -1;
  if (right === 'user') return 1;
  return left.localeCompare(right);
}

function itemLabel(status: string): string {
  if (status === 'available') return 'Available';
  if (status === 'missing') return 'Missing';
  return 'Unknown';
}

function verdictOf(validation: PackValidation): Verdict {
  const statuses = [
    ...(validation.node_classes ?? []).map((item) => item.status),
    ...(validation.models ?? []).map((item) => item.status),
  ];
  if (validation.status === 'missing' || statuses.includes('missing'))
    return 'missing';
  const explicitOk =
    validation.status === 'ok' &&
    (validation.diagnostics ?? []).length === 0 &&
    statuses.every((status) => status === 'available');
  return explicitOk ? 'ok' : 'unknown';
}

function headlineOf(
  verdict: Verdict,
  status: string,
  declared: boolean,
): string {
  if (verdict === 'ok')
    return declared
      ? 'Every declared node class and model is available in ComfyUI.'
      : 'This pack declares no node classes or models, so there was nothing to check.';
  if (verdict === 'missing')
    return 'Something this pack needs is missing. Install it in ComfyUI yourself, then check again.';
  if (status === 'unavailable')
    return 'ComfyUI could not be checked, so nothing here is confirmed.';
  return 'Some requirements could not be checked, so nothing here is confirmed.';
}

const TITLES: Record<Verdict, string> = {
  ok: 'Requirements confirmed',
  missing: 'Requirements missing',
  unknown: 'Requirements not confirmed',
};

export function validationView(validation: PackValidation): ValidationView {
  const status = String(validation.status);
  const verdict = verdictOf(validation);
  const reason = KNOWN_STATUSES.includes(status)
    ? (validation.reason ?? null)
    : `Unexpected status "${status}" from GenAImage.`;
  const nodes = (validation.node_classes ?? []).map((item) => ({
    name: item.class,
    label: itemLabel(item.status),
  }));
  const models = (validation.models ?? []).map((item) => ({
    folder: item.folder,
    name: item.name,
    label: itemLabel(item.status),
    loader: item.loader ?? null,
    reason: item.reason ?? null,
  }));
  return {
    verdict,
    title: TITLES[verdict],
    headline: headlineOf(verdict, status, nodes.length + models.length > 0),
    reason,
    nodes,
    models,
    diagnostics: validation.diagnostics ?? [],
  };
}
