import type { components } from '../../lib/api/schema';

export type DoctorReport = components['schemas']['DoctorReport'];
export type DoctorCheck = components['schemas']['DoctorCheck'];
export type Tone = 'ok' | 'warning' | 'error' | 'skipped';

export const DOCTOR_CHECK_IDS = [
  'server',
  'config',
  'comfyui.reachable',
  'comfyui.version',
  'comfyui.object_info',
  'workflows.nodes',
  'packs.requirements',
  'llm.reachable',
  'llm.text',
  'llm.vision',
  'gpu',
  'disk',
  'ports',
] as const;

export type ReportValidation = {
  valid: boolean;
  missing: string[];
  unexpected: string[];
  duplicated: string[];
};

/** Checks that the server reported each expected check exactly once. */
export function validateReport(report: unknown): ReportValidation {
  const raw = (report as { checks?: unknown } | null | undefined)?.checks;
  const reported = Array.isArray(raw)
    ? raw.map((check) => String((check as { id?: unknown } | null)?.id))
    : [];
  const expected: readonly string[] = DOCTOR_CHECK_IDS;
  const missing = expected.filter((id) => !reported.includes(id));
  const unexpected = [
    ...new Set(reported.filter((id) => !expected.includes(id))),
  ];
  const duplicated = [
    ...new Set(reported.filter((id, at) => reported.indexOf(id) !== at)),
  ];
  return {
    valid:
      Array.isArray(raw) &&
      !missing.length &&
      !unexpected.length &&
      !duplicated.length,
    missing,
    unexpected,
    duplicated,
  };
}

const labels: Record<string, string> = {
  server: 'GenAImage server',
  config: 'Configuration',
  'comfyui.reachable': 'ComfyUI reachable',
  'comfyui.version': 'ComfyUI version',
  'comfyui.object_info': 'ComfyUI node catalogue',
  'workflows.nodes': 'Workflow nodes',
  'packs.requirements': 'Pack requirements',
  'llm.reachable': 'LLM reachable',
  'llm.text': 'LLM text model',
  'llm.vision': 'LLM vision model',
  gpu: 'GPU',
  disk: 'Disk space',
  ports: 'Ports',
};

export function checkLabel(id: string): string {
  return labels[id] ?? id;
}

const statuses: Record<string, { label: string; icon: string; tone: Tone }> = {
  ok: { label: 'OK', icon: '✓', tone: 'ok' },
  warning: { label: 'Warning', icon: '!', tone: 'warning' },
  error: { label: 'Error', icon: '✕', tone: 'error' },
  skipped: { label: 'Skipped', icon: '–', tone: 'skipped' },
};

/** Anything the server did not name as `ok` is never shown as healthy. */
export function describeCheckStatus(status: string) {
  return (
    Object.hasOwn(statuses, status)
      ? statuses[status]
      : { label: 'Unknown status', icon: '?', tone: 'error' as Tone }
  ) satisfies { label: string; icon: string; tone: Tone };
}

export function summarise(checks: readonly DoctorCheck[]) {
  const count = (status: string) =>
    checks.filter((check) => check.status === status).length;
  const ok = count('ok');
  const warnings = count('warning');
  const skipped = count('skipped');
  return {
    ok,
    warnings,
    skipped,
    errors: checks.length - ok - warnings - skipped,
  };
}

const plural = (count: number, noun: string) =>
  `${count} ${noun}${count === 1 ? '' : 's'}`;

export function describeCounts(checks: readonly DoctorCheck[]): string {
  const { ok, errors, warnings, skipped } = summarise(checks);
  return `${ok} of ${checks.length} checks OK · ${plural(errors, 'error')} · ${plural(warnings, 'warning')} · ${skipped} skipped`;
}

export type Segment = { text: string; code: boolean };

/** Splits `text` on backticks so setting keys render as inline code. */
export function splitInlineCode(text: string): Segment[] {
  return text
    .split(/`([^`]+)`/)
    .map((part, index) => ({ text: part, code: index % 2 === 1 }))
    .filter((segment) => segment.text !== '');
}
