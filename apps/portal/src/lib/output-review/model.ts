import type { components } from '../api/schema';

export type OutputAnnotation =
  components['schemas']['OutputAnnotationResponse'];
export type HealthResponse = components['schemas']['HealthResponse'];

export const MAX_VISION_REVIEW_BYTES = 20 * 1024 * 1024;

export function problemToDisabledReason(
  problem: {
    status?: number;
    detail?: string;
    title?: string;
  },
  fallback = 'The configured vision model is unavailable for output review.',
): string | undefined {
  if (problem.status !== 503) return undefined;
  return problem.detail?.trim() || problem.title?.trim() || fallback;
}

export function healthToDisabledReason(
  health: HealthResponse | null | undefined,
  role: 'vision' | 'text' = 'vision',
): string | undefined {
  const status = role === 'text' ? health?.llm_text : health?.llm_vision;
  if (status?.status === 'ok') return undefined;
  const reason = status?.reason?.trim() || health?.llm_reason?.trim();
  const label = role === 'text' ? 'Prompt assistance' : 'Vision review';
  return reason
    ? `${label} is unavailable: ${reason}`
    : `No ${role} model is available for ${role === 'text' ? 'prompt assistance or workflow suggestions' : 'output review'}.`;
}

export function outputReviewMediaDisabledReason(
  contentType: string,
  byteSize: number,
): string | undefined {
  if (!['image/png', 'image/jpeg', 'image/webp'].includes(contentType))
    return 'Vision review supports PNG, JPEG and WebP images only.';
  if (byteSize > MAX_VISION_REVIEW_BYTES)
    return 'Vision review supports images up to 20 MiB.';
  return undefined;
}
