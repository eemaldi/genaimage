<script lang="ts">
  import { untrack } from 'svelte';
  import { ApiHttpError, ApiProblemError, api } from '../api/client';
  import Button from '../ui/Button.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import OutputAnnotationCard from './OutputAnnotationCard.svelte';
  import {
    healthToDisabledReason,
    outputReviewMediaDisabledReason,
    problemToDisabledReason,
    type OutputAnnotation,
  } from './model';

  type ReviewState = {
    outputId: string;
    annotations: OutputAnnotation[];
    annotationsLoading: boolean;
    annotationsProblem?: Problem;
    availability: 'checking' | 'available' | 'unavailable';
    disabledReason?: string;
    settingsHint: boolean;
    availabilityProblem?: Problem;
    reviewing: boolean;
    reviewProblem?: Problem;
  };

  let {
    outputId,
    contentType,
    byteSize,
    initiallyOpen = false,
  }: {
    outputId: string;
    contentType: string;
    byteSize: number;
    initiallyOpen?: boolean;
  } = $props();

  let expanded = $state(untrack(() => initiallyOpen));
  let reviewState = $state<ReviewState>();
  let activeController: AbortController | undefined;
  let current = $derived(
    reviewState?.outputId === outputId ? reviewState : undefined,
  );
  let mediaReason = $derived(
    outputReviewMediaDisabledReason(contentType, byteSize),
  );
  let reviewing = $derived(current?.reviewing ?? false);
  let busy = $derived(
    current?.availability === 'checking' ||
      current?.annotationsLoading === true ||
      reviewing,
  );
  let reviewDisabled = $derived(
    !expanded ||
      !current ||
      current.availability !== 'available' ||
      !!current.disabledReason ||
      !!mediaReason,
  );
  let annotations = $derived(
    current?.annotations.filter((item) => item.output_id === outputId) ?? [],
  );

  function isCurrent(id: string, signal: AbortSignal) {
    return (
      !signal.aborted &&
      expanded &&
      outputId === id &&
      reviewState?.outputId === id
    );
  }

  function update(
    id: string,
    signal: AbortSignal,
    change: (previous: ReviewState) => ReviewState,
  ) {
    if (!isCurrent(id, signal) || !reviewState) return;
    reviewState = change(reviewState);
  }

  function problemFrom(error: unknown, title: string): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    if (error instanceof ApiHttpError) {
      return {
        status: error.status,
        title,
        detail: error.message,
        recovery: 'Check the local GenAImage server and try again.',
      };
    }
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  async function loadAnnotations(id: string, signal: AbortSignal) {
    update(id, signal, (previous) => ({
      ...previous,
      annotationsLoading: true,
      annotationsProblem: undefined,
    }));
    try {
      const result = await api.getOutputAnnotations(id, { signal });
      update(id, signal, (previous) => {
        const existingIds = new Set(previous.annotations.map(({ id }) => id));
        const fromServer = result.annotations.filter(
          (annotation) =>
            annotation.output_id === id && !existingIds.has(annotation.id),
        );
        return {
          ...previous,
          annotations: [...previous.annotations, ...fromServer],
          annotationsLoading: false,
        };
      });
    } catch (error) {
      if (!isCurrent(id, signal)) return;
      update(id, signal, (previous) => ({
        ...previous,
        annotationsLoading: false,
        annotationsProblem: problemFrom(error, 'Could not load model opinions'),
      }));
    }
  }

  async function checkAvailability(id: string, signal: AbortSignal) {
    update(id, signal, (previous) => ({
      ...previous,
      availability: 'checking',
      disabledReason: undefined,
      settingsHint: false,
      availabilityProblem: undefined,
    }));
    try {
      const health = await api.health({ signal });
      const reason = healthToDisabledReason(health);
      update(id, signal, (previous) => ({
        ...previous,
        availability: reason ? 'unavailable' : 'available',
        disabledReason: reason,
        settingsHint: !!reason,
        availabilityProblem: undefined,
        reviewProblem:
          reason === undefined && previous.reviewProblem?.status === 503
            ? undefined
            : previous.reviewProblem,
      }));
    } catch (error) {
      if (!isCurrent(id, signal)) return;
      const problem = problemFrom(error, 'Could not check vision availability');
      const reason =
        problemToDisabledReason(problem) ??
        'Could not check vision availability. Check Settings and try again.';
      update(id, signal, (previous) => ({
        ...previous,
        availability: 'unavailable',
        disabledReason: reason,
        settingsHint: true,
        availabilityProblem: problem,
      }));
    }
  }

  $effect(() => {
    const id = outputId;
    const isExpanded = expanded;
    if (!isExpanded) {
      reviewState = undefined;
      return;
    }

    const controller = new AbortController();
    const { signal } = controller;
    const unsupported = outputReviewMediaDisabledReason(contentType, byteSize);
    activeController = controller;
    reviewState = {
      outputId: id,
      annotations: [],
      annotationsLoading: true,
      availability: unsupported ? 'unavailable' : 'checking',
      disabledReason: unsupported,
      settingsHint: false,
      reviewing: false,
    };
    untrack(() => {
      void loadAnnotations(id, signal);
      if (!unsupported) void checkAvailability(id, signal);
    });

    return () => {
      controller.abort();
      if (activeController === controller) activeController = undefined;
    };
  });

  async function review() {
    const id = outputId;
    const controller = activeController;
    const signal = controller?.signal;
    if (
      !signal ||
      !current ||
      current.outputId !== id ||
      !expanded ||
      current.availability !== 'available' ||
      current.reviewing ||
      mediaReason
    )
      return;

    update(id, signal, (previous) => ({
      ...previous,
      reviewing: true,
      reviewProblem: undefined,
    }));
    try {
      const annotation = await api.reviewOutput(id, { signal });
      if (!isCurrent(id, signal)) return;
      if (annotation.output_id !== id)
        throw new Error(
          'The review response did not match the requested output.',
        );
      update(id, signal, (previous) => ({
        ...previous,
        annotations: [
          annotation,
          ...previous.annotations.filter((item) => item.id !== annotation.id),
        ],
        reviewProblem: undefined,
      }));
    } catch (error) {
      if (!isCurrent(id, signal)) return;
      const problem = problemFrom(error, 'Could not review this output');
      const reason = problemToDisabledReason(problem);
      update(id, signal, (previous) => ({
        ...previous,
        reviewProblem: problem,
        availability: reason ? 'unavailable' : previous.availability,
        disabledReason: reason ?? previous.disabledReason,
        settingsHint: reason ? true : previous.settingsHint,
      }));
    } finally {
      update(id, signal, (previous) => ({ ...previous, reviewing: false }));
    }
  }

  function retryAnnotations() {
    if (activeController)
      void loadAnnotations(outputId, activeController.signal);
  }

  function retryAvailability() {
    if (activeController)
      void checkAvailability(outputId, activeController.signal);
  }
</script>

<details bind:open={expanded} class="review-panel">
  <summary>
    Model opinions <span>(not part of the run record)</span>
  </summary>
  <section aria-label="Output model review" aria-busy={busy}>
    {#if !current}
      <p role="status">Preparing output review…</p>
    {:else}
      <Button
        onclick={review}
        disabled={reviewDisabled}
        aria-disabled={reviewing}
        aria-busy={reviewing}>{reviewing ? 'Reviewing…' : 'Review'}</Button
      >

      {#if mediaReason}
        <p class="disabled-reason">{mediaReason}</p>
      {:else if current.availability === 'checking'}
        <p role="status">Checking vision model availability…</p>
      {:else if current.disabledReason}
        <p class="disabled-reason">
          Review is disabled. {current.disabledReason}
          {#if current.settingsHint}
            Check <a href="#/settings">Settings</a> for a configured vision model.
          {/if}
        </p>
        {#if current.availability === 'unavailable'}<Button
            onclick={retryAvailability}>Check availability again</Button
          >{/if}
      {/if}

      {#if current.availabilityProblem}
        <ProblemDetails problem={current.availabilityProblem} />
      {/if}
      {#if current.reviewProblem}
        <ProblemDetails problem={current.reviewProblem} />
      {/if}
      {#if current.annotationsProblem}
        <ProblemDetails problem={current.annotationsProblem} />
        <Button onclick={retryAnnotations}>Reload model opinions</Button>
      {/if}
      {#if current.reviewing}<p role="status">
          Reviewing this output with the configured vision model…
        </p>{/if}
      {#if current.annotationsLoading}<p role="status">
          Loading model opinions…
        </p>
      {:else if annotations.length}
        <div class="annotations">
          {#each annotations as annotation (annotation.id)}
            <OutputAnnotationCard {annotation} />
          {/each}
        </div>
      {:else if !current.annotationsProblem}
        <p>No model opinions have been recorded for this output.</p>
      {/if}
    {/if}
  </section>
</details>

<style>
  .review-panel {
    margin-top: var(--space-3);
    padding-top: var(--space-3);
    border-top: 1px solid var(--border);
  }
  .review-panel summary {
    cursor: pointer;
    font-weight: var(--weight-strong);
  }
  .review-panel summary span,
  .disabled-reason {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .annotations {
    display: grid;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }
  .review-panel :global(.ui-button[aria-disabled='true']) {
    cursor: progress;
  }
</style>
