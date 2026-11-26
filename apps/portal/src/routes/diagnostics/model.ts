import type { components } from '../../lib/api/schema';

export type DoctorReport = components['schemas']['DoctorReport'];
export type DoctorCheck = components['schemas']['DoctorCheck'];
export type Tone = 'ok' | 'warning' | 'error' | 'skipped';

const labels: Record<string, string> = {
  server: 'GenAImage server',
  config: 'Configuration',
  'comfyui.reachable': 'ComfyUI reachable',
  'comfyui.version': 'ComfyUI version',
  'comfyui.object_info': 'ComfyUI node catalogue',
  'workflows.nodes': 'Workflow nodes',
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

export type Segment = { text: string; code: boolean };

/** Splits `text` on backticks so setting keys render as inline code. */
export function splitInlineCode(text: string): Segment[] {
  return text
    .split(/`([^`]+)`/)
    .map((part, index) => ({ text: part, code: index % 2 === 1 }))
    .filter((segment) => segment.text !== '');
}
