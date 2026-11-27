import type { components } from '../../lib/api/schema';

export type ModelPull = components['schemas']['ModelPull'];
export type LlmModelsResponse = components['schemas']['LlmModelsResponse'];
export type CheckpointsResponse = components['schemas']['CheckpointsResponse'];

const units = ['KB', 'MB', 'GB', 'TB'];

/** Display only: the exact byte count stays in the data. */
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  let value = bytes;
  let unit = -1;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

/** Whole percent, only while the total is known and positive. */
export function percentComplete(pull: ModelPull): number | undefined {
  const { total_bytes: total, completed_bytes: done } = pull;
  if (total == null || total <= 0 || done == null) return undefined;
  return Math.min(100, Math.floor((done / total) * 100));
}

export const isRunning = (pull: ModelPull) => pull.state === 'running';

/** Names the server will accept are its call; this only stops obvious typos. */
export function validateModelName(raw: string): string | undefined {
  const name = raw.trim();
  if (!name) return 'Enter a model name, for example llama3.2:3b.';
  if (name.length > 255) return 'The model name is too long.';
  if (/\s/.test(name)) return 'A model name cannot contain spaces.';
  if (name.includes('://') || name.includes('//'))
    return 'Enter a model name such as llama3.2:3b, not a URL.';
  if (!/^[A-Za-z0-9][A-Za-z0-9._\-/:@]*$/.test(name))
    return 'Use letters, digits and . _ - / : only, starting with a letter or digit.';
}

export const roleStates = {
  present: { label: 'Present', icon: '✓' },
  missing: { label: 'Missing', icon: '✕' },
  not_configured: { label: 'Not configured', icon: '–' },
} as const;

export const pullStates = {
  running: { label: 'Running', icon: '…' },
  succeeded: { label: 'Installed', icon: '✓' },
  failed: { label: 'Failed', icon: '✕' },
  cancelled: { label: 'Cancelled', icon: '–' },
} as const;

/** Splits a step into prose and a trailing `docker …` command. */
export function splitCommand(step: string): { text: string; command?: string } {
  const match = /^(.*?)\s*(docker\s.+)$/.exec(step);
  return match
    ? { text: match[1].replace(/:\s*$/, ''), command: match[2] }
    : { text: step };
}
