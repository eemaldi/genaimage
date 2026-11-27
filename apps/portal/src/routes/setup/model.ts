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
  /** The patch that was saved, so it only counts for identical current choices. */
  saved?: { signature: string };
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

/** Whether the wizard may write this key: neither read-only nor owned by the environment. */
export function isEditable(settings: Settings | undefined, key: string) {
  const setting = settings?.settings[key];
  return !!setting && !setting.read_only && setting.source !== 'env';
}

export function patchSignature(patch: Record<string, string>): string {
  return JSON.stringify(
    Object.entries(patch).sort(([a], [b]) => (a < b ? -1 : 1)),
  );
}

/** Selects an endpoint; a new LLM endpoint invalidates models chosen for the old one. */
export function chooseEndpoint(
  choices: Choices,
  evidence: Evidence,
  kind: 'comfyui' | 'llm',
  url: string | undefined,
) {
  if (kind === 'comfyui') {
    choices.comfyuiUrl = url;
    return;
  }
  if (choices.llmUrl !== url) {
    choices.textModel = undefined;
    choices.visionModel = undefined;
    delete evidence.llm;
  }
  choices.llmUrl = url;
}

export function llmVerified(
  evidence: Evidence,
  choices: Choices,
  llmUrl: string,
): boolean {
  return (
    !!evidence.llm &&
    evidence.llm.url === llmUrl &&
    !!evidence.llm.textModel &&
    (!choices.textModel || choices.textModel === evidence.llm.textModel)
  );
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
    if (!to || !isEditable(settings, key)) continue;
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
        !isEditable(settings, key) &&
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

/**
 * State handed to the test generation step (component C).
 *
 * Component C must not treat any field as evidence that a run works.
 * `checkpoint` and `llmTextModel` are operator choices, not run evidence, and
 * the checkpoint was only listed by the running server. C must not run while
 * `restartPending` could change the ComfyUI endpoint, that is, while
 * `activeComfyuiUrl` differs from or cannot confirm `comfyuiUrl`.
 */
export type TestGenerationState = {
  /** A choice from the installed list; not run evidence. */
  checkpoint?: string;
  /** A choice from the installed list; not run evidence. */
  llmTextModel?: string;
  /** The ComfyUI URL chosen or configured in the wizard (may not be live yet). */
  comfyuiUrl: string;
  /** The ComfyUI URL the running server uses; `undefined` when unknown. */
  activeComfyuiUrl?: string;
  /** True when this session saved exactly the current choices. */
  savedThisSession: boolean;
  /** True when the server reports a config file on disk. */
  configFileExists: boolean;
  /** True when a restart is pending or could not be ruled out. */
  restartPending: boolean;
};
