import { buildProposalGenerateUrl } from '../../routes/generate/share';

export type RunProposal = {
  workflow_id: string;
  workflow_revision: string;
  preset: string | null | undefined;
  parameters: Record<string, unknown>;
  issues?: unknown[];
};

export function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return;
  return value as Record<string, unknown>;
}

export function asRunProposal(value: unknown): RunProposal | undefined {
  const proposal = asRecord(value);
  const parameters = asRecord(proposal?.parameters);
  if (
    typeof proposal?.workflow_id !== 'string' ||
    typeof proposal.workflow_revision !== 'string' ||
    !parameters
  )
    return;
  const preset = proposal.preset;
  if (preset !== undefined && preset !== null && typeof preset !== 'string')
    return;
  return {
    workflow_id: proposal.workflow_id,
    workflow_revision: proposal.workflow_revision,
    preset: preset as string | null | undefined,
    parameters,
    issues: Array.isArray(proposal.issues) ? proposal.issues : undefined,
  };
}

export function generateAction(value: unknown) {
  const proposal = asRunProposal(value);
  if (!proposal) return undefined;
  try {
    return { proposal, href: buildProposalGenerateUrl(proposal) };
  } catch (error) {
    return {
      proposal,
      error:
        error instanceof Error
          ? error.message
          : 'The Generate link could not be created.',
    };
  }
}
