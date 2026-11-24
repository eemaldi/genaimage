<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import type { components } from '../../lib/api/schema';
  import Field from '../../lib/ui/Field.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';

  type Upload = components['schemas']['UploadResponse'];
  let {
    id,
    label,
    hint,
    error,
    value,
    required,
    limit,
    change,
    status,
  }: {
    id: string;
    label: string;
    hint?: string;
    error?: string;
    value?: string;
    required: boolean;
    limit?: number;
    change: (id: string) => void;
    status: (message?: string) => void;
  } = $props();
  let stored = $state.raw<{ metadata: Upload; file: File }>();
  let problem = $state<Problem>();
  let pending = $state(false);
  let previewProblem = $state(false);
  let canvas = $state<HTMLCanvasElement>();
  let current: AbortController | undefined;
  const bytes = new Intl.NumberFormat('en-GB');
  let displayed = $derived(
    stored?.metadata.upload_id === value ? stored : undefined,
  );
  onDestroy(() => current?.abort());

  $effect(() => {
    const target = canvas;
    const file = displayed?.file;
    if (!target || !file) return;
    let cancelled = false;
    previewProblem = false;
    void (async () => {
      try {
        const bitmap = await createImageBitmap(file);
        try {
          if (cancelled) return;
          const scale = Math.min(
            1,
            480 / Math.max(bitmap.width, bitmap.height),
          );
          target.width = Math.max(1, Math.round(bitmap.width * scale));
          target.height = Math.max(1, Math.round(bitmap.height * scale));
          const context = target.getContext('2d');
          if (!context) throw new Error('Canvas is unavailable.');
          context.drawImage(bitmap, 0, 0, target.width, target.height);
        } finally {
          bitmap.close();
        }
      } catch {
        if (!cancelled) previewProblem = true;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function readSignature(file: File) {
    return new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () =>
        reject(new Error('Could not read the image file.'));
      reader.onload = () =>
        resolve(
          String.fromCharCode(...new Uint8Array(reader.result as ArrayBuffer)),
        );
      reader.readAsArrayBuffer(file.slice(0, 12));
    });
  }

  async function select(event: Event & { currentTarget: HTMLInputElement }) {
    const file = event.currentTarget.files?.[0];
    if (!file) return;
    event.currentTarget.value = '';
    current?.abort();
    const controller = new AbortController();
    current = controller;
    pending = true;
    problem = undefined;
    status('Wait for the image upload to finish.');
    try {
      // The server caps the complete multipart request, not just the image bytes.
      if (limit !== undefined && file.size >= limit)
        throw new Error(
          `Choose a smaller image. The upload request limit is ${bytes.format(limit)} bytes.`,
        );
      const signature = await readSignature(file);
      if (
        !signature.startsWith('\x89PNG\r\n\x1a\n') &&
        !signature.startsWith('\xff\xd8\xff') &&
        !(signature.startsWith('RIFF') && signature.slice(8, 12) === 'WEBP')
      )
        throw new Error('Choose a PNG, JPEG or WebP image.');
      controller.signal.throwIfAborted();
      const metadata = await api.upload(file, { signal: controller.signal });
      controller.signal.throwIfAborted();
      stored = { metadata, file };
      change(metadata.upload_id);
      status();
    } catch (failure) {
      if (controller.signal.aborted) return;
      problem =
        failure instanceof ApiProblemError
          ? failure.problem
          : {
              title: 'Could not upload the image',
              detail: failure instanceof Error ? failure.message : undefined,
            };
      status(problem.detail || problem.title || 'Could not upload the image.');
    } finally {
      if (!controller.signal.aborted) pending = false;
    }
  }
</script>

<Field
  {id}
  {label}
  {error}
  hint={[
    hint,
    'Choose PNG, JPEG or WebP.',
    limit === undefined
      ? 'The server enforces its configured upload size limit.'
      : `Upload request limit: ${bytes.format(limit)} bytes, including form data.`,
  ]
    .filter(Boolean)
    .join(' ')}
>
  {#snippet children(attrs)}
    <input
      {...attrs}
      class="ui-control"
      type="file"
      accept="image/png,image/jpeg,image/webp"
      aria-required={required}
      onchange={select}
    />
  {/snippet}
</Field>
{#if pending}<p role="status">Uploading image…</p>{/if}
{#if problem}<ProblemDetails {problem} />{/if}
{#if value}
  <p>Upload ID: <code>{value}</code></p>
{/if}
{#if displayed}
  <p>
    {displayed.metadata.original_filename} · {displayed.metadata.media_type} · {bytes.format(
      displayed.metadata.byte_size,
    )} bytes
  </p>
  <div
    role="img"
    aria-label={`Preview of ${displayed.metadata.original_filename}`}
    hidden={previewProblem}
  >
    <canvas bind:this={canvas} aria-hidden="true"></canvas>
  </div>
  {#if previewProblem}<p>
      The browser could not display this image preview.
    </p>{/if}
{/if}

<style>
  code {
    overflow-wrap: anywhere;
  }
  canvas {
    max-width: 100%;
    height: auto;
  }
</style>
