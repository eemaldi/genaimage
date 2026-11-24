import type { components } from '../api/schema';

export type OutputAnnotation =
  components['schemas']['OutputAnnotationResponse'];
export type HealthResponse = components['schemas']['HealthResponse'];

export const MAX_VISION_REVIEW_BYTES = 20 * 1024 * 1024;

export function problemToDisabledReason(problem: {
  status?: number;
  detail?: string;
  title?: string;
}): string | undefined {
  if (problem.status !== 503) return undefined;
  return (
    problem.detail?.trim() ||
    problem.title?.trim() ||
    'The configured vision model is unavailable for output review.'
  );
}

export function healthToDisabledReason(
  health: HealthResponse | null | undefined,
): string | undefined {
  if (health?.llm_vision?.status === 'ok') return undefined;
  const reason =
    health?.llm_vision?.reason?.trim() || health?.llm_reason?.trim();
  return reason
    ? `Vision review is unavailable: ${reason}`
    : 'No vision model is available for output review.';
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
