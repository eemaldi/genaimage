import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiProblemError } from '../api/client';
import type { components } from '../api/schema';
import OutputReviewPanel from './OutputReviewPanel.svelte';

const fake = vi.hoisted(() => ({
  api: {
    getOutputAnnotations: vi.fn(),
    health: vi.fn(),
    reviewOutput: vi.fn(),
  },
}));
vi.mock('../api/client', async (original) => ({
  ...(await original<typeof import('../api/client')>()),
  api: fake.api,
}));

const date = '2026-03-01T10:15:00Z';
function annotation(
  outputId: string,
  id = 1,
  caption = 'A quiet coast at dusk.',
): components['schemas']['OutputAnnotationResponse'] {
  return {
    id,
    output_id: outputId,
    run_id: 'run-1',
    kind: 'vision_review',
    model: 'vision-local-v1',
    created_at: date,
    caption,
    adherence_critique: 'The horizon is slightly higher than requested.',
    tags: ['coast', 'dusk'],
    prompt: 'Review the submitted output.',
    raw_response: '{"caption":"A quiet coast at dusk."}',
  };
}

const media = {
  contentType: 'image/png',
  byteSize: 1024,
  initiallyOpen: true,
};

beforeEach(() => {
  vi.resetAllMocks();
  fake.api.getOutputAnnotations.mockResolvedValue({ annotations: [] });
  fake.api.health.mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_vision: { status: 'ok', model: 'vision-local-v1' },
  });
  fake.api.reviewOutput.mockImplementation(async (outputId: string) =>
    annotation(outputId),
  );
});

afterEach(cleanup);

describe('output review panel', () => {
  it('lists a spec review as a model-opinion checklist', async () => {
    fake.api.getOutputAnnotations.mockResolvedValue({
      annotations: [
        {
          ...annotation('run-1:1', 7, 'Close match'),
          kind: 'spec_review',
          adherence_critique: '',
          tags: [],
          spec_review: {
            character_id: 'char-1',
            spec_version: 2,
            checklist: [
              {
                id: 'cloak',
                category: 'clothing',
                requirement: 'mandatory',
                verdict: 'deviation',
                note: 'Green',
              },
              {
                id: 'hat',
                category: 'clothing',
                requirement: 'mandatory',
                verdict: 'unclear',
                note: 'not assessed by the model',
              },
            ],
            dropped: ['unknown element ids dropped: boots'],
            summary: 'Close match',
            label: 'Model opinion, not approval.',
          },
        },
      ],
    });
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });

    const list = await screen.findByRole('list', { name: 'Spec checklist' });

    const rows = within(list).getAllByRole('listitem');
    expect(
      rows.map((row) => row.textContent?.replace(/\s+/g, ' ').trim()),
    ).toEqual([
      'cloak (mandatory, clothing) · deviation · Green',
      'hat (mandatory, clothing) · unclear · not assessed by the model',
    ]);
    expect(screen.getByText('unknown element ids dropped: boots')).toBeTruthy();
    expect(screen.getByText(/Model opinion, not approval\./)).toBeTruthy();
    expect(screen.queryByText('Adherence critique')).toBeNull();
  });

  it('renders duplicate dropped notes without crashing', async () => {
    fake.api.getOutputAnnotations.mockResolvedValue({
      annotations: [
        {
          ...annotation('run-1:1', 7, 'Close match'),
          kind: 'spec_review',
          adherence_critique: '',
          tags: [],
          spec_review: {
            character_id: 'char-1',
            spec_version: 1,
            checklist: [],
            dropped: ['same note', 'same note', 'same note'],
            summary: 'Close match',
            label: 'Model opinion, not approval.',
          },
        },
      ],
    });
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });

    expect(await screen.findAllByText('same note')).toHaveLength(3);
  });

  it('renders a successful review as a labelled model opinion', async () => {
    const user = userEvent.setup();
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });
    await screen.findByText(
      'No model opinions have been recorded for this output.',
    );
    await user.click(screen.getByRole('button', { name: 'Review' }));

    const card = await screen.findByRole('article', {
      name: 'Model opinion by vision-local-v1',
    });
    expect(within(card).getByText('A quiet coast at dusk.')).toBeTruthy();
    expect(
      within(card).getByText('The horizon is slightly higher than requested.'),
    ).toBeTruthy();
    expect(within(card).getByText('coast, dusk')).toBeTruthy();
    expect(card.querySelector('.label')?.textContent).toContain(
      'Model opinion · vision-local-v1 ·',
    );
    expect(
      within(card).getByText('Details', { selector: 'summary' }),
    ).toBeTruthy();
    expect(fake.api.reviewOutput).toHaveBeenCalledWith(
      'run-1:1',
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
  });

  it('disables review with the problem reason after a 503', async () => {
    const user = userEvent.setup();
    fake.api.reviewOutput.mockRejectedValue(
      new ApiProblemError({
        status: 503,
        type: 'about:blank',
        title: 'Vision review unavailable',
        detail: 'The configured vision model is not available.',
        recovery: 'Configure the vision role in Settings.',
      }),
    );
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });
    await screen.findByText(
      'No model opinions have been recorded for this output.',
    );
    const review = screen.getByRole('button', { name: 'Review' });
    await user.click(review);

    expect(
      await screen.findByText('The configured vision model is not available.'),
    ).toBeTruthy();
    expect((review as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole('link', { name: 'Settings' })).toBeTruthy();
  });

  it('does not submit twice while a review is pending', async () => {
    const user = userEvent.setup();
    let finish!: (
      result: components['schemas']['OutputAnnotationResponse'],
    ) => void;
    fake.api.reviewOutput.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });
    await screen.findByText(
      'No model opinions have been recorded for this output.',
    );
    const review = screen.getByRole('button', { name: 'Review' });
    await user.click(review);
    expect(review.textContent).toContain('Reviewing');
    expect(review.getAttribute('aria-disabled')).toBe('true');
    await fireEvent.click(review);
    expect(fake.api.reviewOutput).toHaveBeenCalledTimes(1);

    finish(annotation('run-1:1'));
    expect(
      await screen.findByRole('article', {
        name: 'Model opinion by vision-local-v1',
      }),
    ).toBeTruthy();
  });

  it('ignores late annotation and review responses after switching outputs', async () => {
    const user = userEvent.setup();
    let finishAnnotations!: (result: {
      annotations: components['schemas']['OutputAnnotationResponse'][];
    }) => void;
    let finishReview!: (
      result: components['schemas']['OutputAnnotationResponse'],
    ) => void;
    fake.api.getOutputAnnotations.mockImplementation((outputId: string) =>
      outputId === 'first'
        ? new Promise((resolve) => {
            finishAnnotations = resolve;
          })
        : Promise.resolve({
            annotations: [annotation('second', 2, 'Current output.')],
          }),
    );
    fake.api.reviewOutput.mockImplementation(
      () =>
        new Promise((resolve) => {
          finishReview = resolve;
        }),
    );

    const view = render(OutputReviewPanel, {
      outputId: 'first',
      ...media,
    });
    const review = await screen.findByRole('button', { name: 'Review' });
    await waitFor(() =>
      expect((review as HTMLButtonElement).disabled).toBe(false),
    );
    await user.click(review);
    expect(fake.api.reviewOutput).toHaveBeenCalledTimes(1);

    await view.rerender({ outputId: 'second', ...media });
    expect(await screen.findByText('Current output.')).toBeTruthy();
    finishAnnotations({ annotations: [annotation('first', 3, 'Stale list.')] });
    finishReview(annotation('first', 4, 'Stale review.'));
    await waitFor(() => {
      expect(screen.queryByText('Stale list.')).toBeNull();
      expect(screen.queryByText('Stale review.')).toBeNull();
    });
    expect(screen.getByText('Current output.')).toBeTruthy();
  });

  it('renders model strings as text, including markup-looking content', async () => {
    const user = userEvent.setup();
    const payload = '<img onerror=alert(1)>';
    fake.api.reviewOutput.mockResolvedValue(annotation('run-1:1', 5, payload));
    const view = render(OutputReviewPanel, { outputId: 'run-1:1', ...media });
    await screen.findByText(
      'No model opinions have been recorded for this output.',
    );
    await user.click(screen.getByRole('button', { name: 'Review' }));
    const card = await screen.findByRole('article', {
      name: 'Model opinion by vision-local-v1',
    });
    expect(within(card).getByText(payload)).toBeTruthy();
    expect(view.container.querySelector('img')).toBeNull();
    await user.click(
      within(card).getByText('Details', { selector: 'summary' }),
    );
    expect(within(card).getByText('Review the submitted output.')).toBeTruthy();
    expect(view.container.querySelector('img')).toBeNull();
  });

  it('keeps review disabled and points to Settings when health has no vision model', async () => {
    fake.api.health.mockResolvedValue({
      self: 'ok',
      image_backend: 'ok',
      llm: 'unavailable',
      llm_vision: { status: 'not_configured', reason: 'Vision role is empty.' },
    });
    render(OutputReviewPanel, { outputId: 'run-1:1', ...media });
    const review = await screen.findByRole('button', { name: 'Review' });
    expect((review as HTMLButtonElement).disabled).toBe(true);
    expect(await screen.findByText(/Vision role is empty/)).toBeTruthy();
    expect(screen.getByRole('link', { name: 'Settings' })).toBeTruthy();
    expect(fake.api.reviewOutput).not.toHaveBeenCalled();
  });
});
