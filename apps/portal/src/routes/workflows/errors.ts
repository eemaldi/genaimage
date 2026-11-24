import { ApiProblemError } from '../../lib/api/client';
import type { Problem } from '../../lib/ui/ProblemDetails.svelte';

export function workflowProblem(error: unknown): Problem {
  if (error instanceof ApiProblemError) return error.problem;
  return {
    title: 'Could not load workflow information',
    detail:
      error instanceof Error ? error.message : 'An unexpected error occurred.',
    recovery: 'Check that the local GenAImage server is running and try again.',
  };
}
