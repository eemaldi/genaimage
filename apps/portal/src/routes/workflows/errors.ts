import { ApiProblemError } from '../../lib/api/client';
import type { Problem } from '../../lib/ui/ProblemDetails.svelte';

const AMBIGUOUS_WORKFLOW = 'urn:genaimage:problem:workflow-ambiguous';

export function workflowProblem(
  error: unknown,
  title = 'Could not load workflow information',
): Problem {
  if (error instanceof ApiProblemError) {
    return error.type === AMBIGUOUS_WORKFLOW
      ? {
          ...error.problem,
          title: 'This workflow ID is declared more than once',
        }
      : error.problem;
  }
  return {
    title,
    detail:
      error instanceof Error ? error.message : 'An unexpected error occurred.',
    recovery: 'Check that the local GenAImage server is running and try again.',
  };
}
