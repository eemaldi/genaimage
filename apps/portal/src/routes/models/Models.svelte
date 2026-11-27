<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiProblemError } from '../../lib/api/client';
  import Button from '../../lib/ui/Button.svelte';
  import Dialog from '../../lib/ui/Dialog.svelte';
  import Input from '../../lib/ui/Input.svelte';
  import ProblemDetails, {
    type Problem,
  } from '../../lib/ui/ProblemDetails.svelte';
  import type { RouteProps } from '../../router';
  import Checkpoints from './Checkpoints.svelte';
  import ModelPullProgress from './ModelPullProgress.svelte';
  import {
    formatBytes,
    isRunning,
    reconnectDelay,
    roleStates,
    validateModelName,
    type LlmModelsResponse,
    type ModelPull,
  } from './model';

  let { params: _params, query: _query }: RouteProps = $props();

  const MAX_SILENT_RECONNECTS = 5;
  const inputId = 'model-pull-name';

  let models = $state<LlmModelsResponse>();
  let modelsProblem = $state<Problem>();
  let modelsLoading = $state(true);
  let pulls = $state<ModelPull[]>([]);
  let pullsProblem = $state<Problem>();
  let unknown = $state<Record<string, true>>({});
  let detached = $state<Record<string, true>>({});
  let cancelling = $state<Record<string, true>>({});
  let name = $state('');
  let nameError = $state<string>();
  let confirmOpen = $state(false);
  let confirming = $state('');
  let starting = $state(false);
  let startProblem = $state<Problem>();
  let startNote = $state('');
  let announcement = $state('');

  const life = new AbortController();
  const following: Record<string, true> = {};
  const settled: Record<string, true> = {};
  let latestModels = 0;

  function problemFrom(error: unknown, title: string, recovery: string) {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery,
    } satisfies Problem;
  }

  const notFound = (error: unknown) =>
    error instanceof ApiProblemError && error.status === 404;

  async function loadModels() {
    const mine = ++latestModels;
    modelsLoading = true;
    try {
      const next = await api.llmModels({ signal: life.signal });
      if (mine !== latestModels) return;
      models = next;
      modelsProblem = undefined;
    } catch (error) {
      if (mine !== latestModels || life.signal.aborted) return;
      models = undefined;
      modelsProblem = problemFrom(
        error,
        'Could not list the LLM models',
        'Check the LLM settings and that the LLM server is running, then try again.',
      );
    } finally {
      if (mine === latestModels) modelsLoading = false;
    }
  }

  /** Keeps the newest snapshot: an older sequence never overwrites a newer one. */
  function apply(pull: ModelPull) {
    const index = pulls.findIndex((known) => known.id === pull.id);
    if (index === -1) pulls = [pull, ...pulls];
    else if (pull.sequence >= pulls[index].sequence) pulls[index] = pull;
    else return pulls[index];
    return pull;
  }

  function settle(pull: ModelPull) {
    if (isRunning(pull) || settled[pull.id]) return;
    settled[pull.id] = true;
    if (pull.state === 'succeeded') {
      announcement = `${pull.name} was pulled and is installed.`;
      void loadModels();
    } else if (pull.state === 'cancelled') {
      announcement = `Pull of ${pull.name} was cancelled. It may still have installed the model.`;
      void loadModels();
    } else if (pull.state === 'failed') {
      announcement = `Pull of ${pull.name} failed${pull.error ? `: ${pull.error}` : '.'}`;
    } else {
      announcement = `Pull of ${pull.name} ended in an unknown state. Check the installed models.`;
    }
  }

  function markUnknown(id: string) {
    unknown[id] = true;
    delete detached[id];
    void loadModels();
  }

  const sleep = (ms: number) =>
    new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer);
        life.signal.removeEventListener('abort', done);
        resolve();
      };
      const timer = setTimeout(done, ms);
      life.signal.addEventListener('abort', done, { once: true });
    });

  async function follow(id: string) {
    if (following[id]) return;
    following[id] = true;
    delete detached[id];
    let silent = 0;
    const known = pulls.find((pull) => pull.id === id);
    let seen = { sequence: known?.sequence ?? -1, state: known?.state };
    /** Progress means a newer sequence or a changed state, never just a frame. */
    const advanced = (pull: ModelPull) => {
      const moved = pull.sequence > seen.sequence || pull.state !== seen.state;
      if (pull.sequence >= seen.sequence)
        seen = { sequence: pull.sequence, state: pull.state };
      return moved;
    };
    try {
      while (!life.signal.aborted) {
        let progressed = false;
        try {
          const stream = await api.streamModelPull(id, {
            signal: life.signal,
          });
          for await (const { data } of stream) {
            if (data?.id !== id || typeof data.sequence !== 'number') continue;
            if (advanced(data)) progressed = true;
            const current = apply(data);
            if (!isRunning(current)) return settle(current);
          }
        } catch (error) {
          if (life.signal.aborted) return;
          if (notFound(error)) return markUnknown(id);
        }
        // A stream that ended without a terminal event was cut: ask again.
        try {
          const current = apply(
            await api.getModelPull(id, { signal: life.signal }),
          );
          if (advanced(current)) progressed = true;
          if (!isRunning(current)) return settle(current);
        } catch (error) {
          if (life.signal.aborted) return;
          if (notFound(error)) return markUnknown(id);
        }
        silent = progressed ? 0 : silent + 1;
        if (silent >= MAX_SILENT_RECONNECTS) {
          detached[id] = true;
          return;
        }
        await sleep(reconnectDelay(silent));
      }
    } finally {
      delete following[id];
    }
  }

  async function loadPulls() {
    try {
      const list = await api.listModelPulls({ signal: life.signal });
      for (const pull of [...list.pulls].reverse()) apply(pull);
      pullsProblem = undefined;
      for (const pull of list.pulls) if (isRunning(pull)) void follow(pull.id);
    } catch (error) {
      if (life.signal.aborted) return;
      pullsProblem = problemFrom(
        error,
        'Could not list the pulls',
        'Check the LLM settings, then reload this page.',
      );
    }
  }

  async function checkAgain(id: string) {
    try {
      const current = apply(
        await api.getModelPull(id, { signal: life.signal }),
      );
      if (isRunning(current)) void follow(id);
      else settle(current);
    } catch (error) {
      if (life.signal.aborted) return;
      if (notFound(error)) markUnknown(id);
    }
  }

  function requestPull() {
    nameError = validateModelName(name);
    if (nameError) return;
    confirming = name.trim();
    confirmOpen = true;
  }

  async function startPull() {
    if (starting) return;
    const wanted = confirming;
    confirmOpen = false;
    starting = true;
    startProblem = undefined;
    startNote = '';
    try {
      const { pull, existingId } = await api.startModelPull(wanted, {
        signal: life.signal,
      });
      name = '';
      if (pull) {
        apply(pull);
        startNote = `Started pulling ${pull.name}.`;
        void follow(pull.id);
      } else if (existingId) {
        startNote = `${wanted} is already being pulled. Showing that pull.`;
        void checkAgain(existingId);
        void follow(existingId);
      }
    } catch (error) {
      if (life.signal.aborted) return;
      startProblem = problemFrom(
        error,
        'Could not start the pull',
        'Check the list of pulls below before trying again.',
      );
      if (!(error instanceof ApiProblemError)) void loadPulls();
    } finally {
      starting = false;
    }
  }

  async function cancel(pull: ModelPull) {
    cancelling[pull.id] = true;
    try {
      settle(
        apply(await api.cancelModelPull(pull.id, { signal: life.signal })),
      );
    } catch (error) {
      if (life.signal.aborted) return;
      if (notFound(error)) markUnknown(pull.id);
      else
        startProblem = problemFrom(
          error,
          'Could not cancel the pull',
          'Check the status of the pull and try again.',
        );
    } finally {
      delete cancelling[pull.id];
    }
  }

  onMount(() => {
    void loadModels();
    void loadPulls();
    return () => life.abort();
  });
</script>

<h1>Models</h1>
<p>
  Nothing is downloaded unless you start a pull here. GenAImage never chooses or
  substitutes a model for you.
</p>
<div role="status" class="visually-hidden">{announcement}</div>

<section aria-labelledby="llm-heading">
  <h2 id="llm-heading">LLM models</h2>
  {#if modelsLoading && !models}<p role="status">Loading LLM models…</p>{/if}
  {#if modelsProblem}
    <ProblemDetails problem={modelsProblem} />
    <p><Button onclick={loadModels}>Try again</Button></p>
  {/if}
  {#if models}
    <p>
      Listed by: <strong>{models.source}</strong>{modelsLoading
        ? ' (refreshing…)'
        : ''}
    </p>
    <h3>Configured roles</h3>
    <ul class="roles">
      {#each models.roles as role (role.role)}
        {@const state = roleStates[role.state]}
        <li data-role={role.role}>
          <strong>{role.role === 'text' ? 'Text' : 'Vision'} model:</strong>
          {role.model ?? 'none'}
          <span class="badge"
            ><span aria-hidden="true">{state.icon}</span> {state.label}</span
          >
          {#if role.state === 'missing' && role.model && models.pull_supported}
            {@const wanted = role.model}
            <Button
              onclick={() => {
                name = wanted;
                nameError = undefined;
                document.getElementById(inputId)?.focus();
              }}>Fill the pull form with {wanted}</Button
            >
          {/if}
        </li>
      {/each}
    </ul>
    <h3>Installed models ({models.models.length})</h3>
    {#if models.models.length}
      <table>
        <thead>
          <tr><th>Name</th><th>Size</th><th>Modified</th></tr>
        </thead>
        <tbody>
          {#each models.models as model (model.name)}
            <tr>
              <td><code>{model.name}</code></td>
              <td
                >{model.size_bytes == null
                  ? 'Not reported'
                  : formatBytes(model.size_bytes)}</td
              >
              <td>{model.modified_at ?? 'Not reported'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p>No models are installed.</p>
    {/if}
  {/if}
</section>

<section aria-labelledby="pull-heading">
  <h2 id="pull-heading">Pull a model</h2>
  {#if models && !models.pull_supported}
    <p>
      <strong>Pulling is not available.</strong>
      {models.pull_unsupported_reason ?? ''}
    </p>
  {/if}
  <form
    onsubmit={(event) => {
      event.preventDefault();
      requestPull();
    }}
    novalidate
  >
    <Input
      id={inputId}
      label="Model name"
      hint="For example llama3.2:3b. Browse the Ollama library to choose one."
      bind:value={name}
      error={nameError}
      disabled={!models?.pull_supported || starting}
      autocomplete="off"
    />
    <p>
      <Button
        variant="primary"
        type="submit"
        disabled={!models?.pull_supported || starting}>Pull…</Button
      >
    </p>
  </form>
  {#if startNote}<p role="status">{startNote}</p>{/if}
  {#if startProblem}<ProblemDetails problem={startProblem} />{/if}
</section>

<section aria-labelledby="pulls-heading">
  <h2 id="pulls-heading">Pulls</h2>
  {#if pullsProblem}
    <ProblemDetails problem={pullsProblem} />
    <p><Button onclick={loadPulls}>Try again</Button></p>
  {:else if !pulls.length}
    <p>
      No pulls since the server started. This does not mean a model is installed
      or missing.
    </p>
  {/if}
  <ul class="pulls">
    {#each pulls as pull (pull.id)}
      <li data-pull={pull.id}>
        <h3><code>{pull.name}</code></h3>
        {#if unknown[pull.id]}
          <p>
            <strong>Unknown</strong> (the server may have restarted). Check the installed
            models above.
          </p>
        {:else}
          <ModelPullProgress {pull} />
          {#if detached[pull.id]}
            <p>
              <strong>Lost contact with the server.</strong> The pull may still
              be running.
              <Button onclick={() => checkAgain(pull.id)}>Check again</Button>
            </p>
          {/if}
          {#if isRunning(pull)}
            <p>
              <Button
                onclick={() => cancel(pull)}
                disabled={cancelling[pull.id]}
                aria-label={`Cancel pull of ${pull.name}`}>Cancel</Button
              >
              A cancelled pull may still have installed the model.
            </p>
          {/if}
        {/if}
      </li>
    {/each}
  </ul>
</section>

<Checkpoints />

<Dialog
  bind:open={confirmOpen}
  title={confirming ? `Pull ${confirming}?` : 'Pull a model?'}
  description={`This will download ${confirming} from the configured Ollama server. The size is not known until the download starts. Nothing is downloaded until you confirm.`}
  returnFocus={() => document.getElementById(inputId)?.focus()}
>
  <p>
    <Button onclick={() => (confirmOpen = false)}>Do not download</Button>
    <Button variant="primary" onclick={startPull}>Download {confirming}</Button>
  </p>
</Dialog>

<style>
  .roles,
  .pulls {
    list-style: none;
    padding: 0;
    display: grid;
    gap: var(--space-3);
  }
  .pulls li {
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    padding: var(--space-3) var(--space-4);
  }
  .badge {
    font-weight: var(--weight-strong);
    margin-inline: var(--space-2);
  }
  table {
    border-collapse: collapse;
    inline-size: 100%;
  }
  th,
  td {
    text-align: start;
    padding: var(--space-2);
    border-block-end: 1px solid var(--border);
  }
</style>
