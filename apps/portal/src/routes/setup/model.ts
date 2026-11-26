import { ApiProblemError } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import type { Problem } from '../../lib/ui/ProblemDetails.svelte';
import { display, type Settings } from '../settings/form';

export type SetupStatus = components['schemas']['SetupStatus'];
export type DetectedService = components['schemas']['DetectedService'];
export type LlmModelsResponse = components['schemas']['LlmModelsResponse'];
export type CheckpointsResponse = components['schemas']['CheckpointsResponse'];

export type StepId =
  'detect' | 'comfyui' | 'llm' | 'checkpoint' | 'review' | 'apply' | 'test';

export const steps: readonly { id: StepId; label: string }[] = [
  { id: 'detect', label: 'Detect services' },
  { id: 'comfyui', label: 'Test ComfyUI' },
  { id: 'llm', label: 'LLM and models' },
  { id: 'checkpoint', label: 'Checkpoint' },
  { id: 'review', label: 'Review and save' },
  { id: 'apply', label: 'Apply changes' },
  { id: 'test', label: 'Test generation' },
];

/** What the operator has chosen so far. `undefined` means "keep as configured". */
export type Choices = {
  comfyuiUrl?: string;
  llmUrl?: string;
  llmSkipped: boolean;
  textModel?: string;
  visionModel?: string;
  checkpoint?: string;
};

/** Explicit success evidence for each step. A step is never done without it. */
export type Evidence = {
  detected?: true;
  comfyui?: { url: string };
  llm?: { url: string; textModel: string };
  checkpoint?: { name: string };
  saved?: true;
};

export type ReviewEntry = {
  key: string;
  label: string;
  from: string;
  to: string;
};

export function latest() {
  let sequence = 0;
  let controller: AbortController | undefined;
  return {
    begin() {
      controller?.abort();
      const mine = ++sequence;
      const current = (controller = new AbortController());
      return { signal: current.signal, live: () => mine === sequence };
    },
    cancel() {
      sequence++;
      controller?.abort();
    },
  };
}

export function problemFrom(
  error: unknown,
  title: string,
  recovery: string,
): Problem {
  if (error instanceof ApiProblemError)
    return { ...error.problem, recovery: error.problem.recovery || recovery };
  return {
    title,
    detail: error instanceof Error ? error.message : undefined,
    recovery,
  };
}

export function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === 'AbortError';
}

export function envVariable(
  settings: Settings | undefined,
  key: string,
): string | undefined {
  const setting = settings?.settings[key];
  if (setting && 'variable' in setting) return String(setting.variable);
}

export function readOnly(settings: Settings | undefined, key: string) {
  return settings?.settings[key]?.read_only ?? true;
}

export function endpointKey(service: 'comfyui' | 'llm') {
  return `${service}.base_url`;
}

export function configuredValue(settings: Settings | undefined, key: string) {
  const setting = settings?.settings[key];
  return setting ? display(setting.value) : '';
}

export function effectiveUrl(
  service: 'comfyui' | 'llm',
  choices: Choices,
  settings: Settings | undefined,
): string {
  return (
    (service === 'comfyui' ? choices.comfyuiUrl : choices.llmUrl) ??
    configuredValue(settings, endpointKey(service))
  );
}

/** Builds exactly what will be saved, so the review screen can show every value. */
export function buildReview(
  choices: Choices,
  settings: Settings,
): { entries: ReviewEntry[]; patch: Record<string, string> } {
  const wanted: [string, string, string | undefined][] = [
    [
      'comfyui.base_url',
      'ComfyUI URL',
      effectiveUrl('comfyui', choices, settings),
    ],
    ...(choices.llmSkipped
      ? []
      : ([
          ['llm.base_url', 'LLM URL', effectiveUrl('llm', choices, settings)],
          ['llm.text_model', 'Text model', choices.textModel],
          ['llm.vision_model', 'Vision model', choices.visionModel],
        ] as [string, string, string | undefined][])),
  ];
  const entries: ReviewEntry[] = [];
  const patch: Record<string, string> = {};
  for (const [key, label, to] of wanted) {
    if (!to || readOnly(settings, key)) continue;
    patch[key] = to;
    entries.push({ key, label, from: configuredValue(settings, key), to });
  }
  return { entries, patch };
}

/** Keys the wizard cannot change because the environment owns them. */
export function envOwned(
  settings: Settings,
  choices: Choices,
): { key: string; label: string; value: string; variable?: string }[] {
  const keys: [string, string][] = [
    ['comfyui.base_url', 'ComfyUI URL'],
    ['llm.base_url', 'LLM URL'],
    ['llm.text_model', 'Text model'],
    ['llm.vision_model', 'Vision model'],
  ];
  return keys
    .filter(
      ([key]) =>
        readOnly(settings, key) &&
        (key.endsWith('base_url') ||
          (!choices.llmSkipped &&
            (key === 'llm.text_model'
              ? choices.textModel
              : choices.visionModel))),
    )
    .map(([key, label]) => ({
      key,
      label,
      value: configuredValue(settings, key),
      variable: envVariable(settings, key),
    }));
}

const dismissalKey = 'genaimage.setup.dismissed';

/** Identifies the current blocking problems so a new problem prompts again. */
export function blockingSignature(status: SetupStatus): string {
  return status.reasons
    .filter((reason) => reason.blocking)
    .map((reason) => reason.code)
    .sort()
    .join(',');
}

export function isDismissed(status: SetupStatus): boolean {
  try {
    return localStorage.getItem(dismissalKey) === blockingSignature(status);
  } catch {
    return false;
  }
}

export function rememberDismissal(status: SetupStatus | undefined) {
  try {
    if (status?.needs_setup)
      localStorage.setItem(dismissalKey, blockingSignature(status));
    else localStorage.removeItem(dismissalKey);
  } catch {
    /* The server status stays authoritative without storage. */
  }
}

export function comfyuiFix(reason: string | null | undefined): string {
  return reason === 'timeout'
    ? 'ComfyUI did not answer in time. Check that it has finished starting and is not busy, then test again.'
    : 'Start ComfyUI and check that the URL matches the address it listens on (default http://127.0.0.1:8188), then test again.';
}

/** State handed to the test generation step (component C). */
export type TestGenerationState = {
  checkpoint?: string;
  comfyuiUrl: string;
  llmTextModel?: string;
  configSaved: boolean;
  restartPending: boolean;
};
