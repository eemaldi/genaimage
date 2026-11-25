<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { api, ApiHttpError, ApiProblemError } from '../api/client';
  import type { AssistantStreamEvent } from '../api/client';
  import {
    healthToDisabledReason,
    problemToDisabledReason,
  } from '../output-review/model';
  import Button from '../ui/Button.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import ProblemDetails, { type Problem } from '../ui/ProblemDetails.svelte';
  import ConfirmationCard from './ConfirmationCard.svelte';
  import Markdown from './Markdown.svelte';
  import ToolCallCard from './ToolCallCard.svelte';
  import ToolResultCard from './ToolResultCard.svelte';
  import { messageItems, type Conversation, type MessageItem } from './model';

  let {
    presentation = 'dock',
    side = 'right',
    close,
    setSide,
  }: {
    presentation?: 'dock' | 'full';
    side?: 'left' | 'right';
    close?: () => void;
    setSide?: (side: 'left' | 'right') => void;
  } = $props();

  let root: HTMLElement;
  let transcript: HTMLElement;
  let conversations = $state<Conversation[]>([]);
  let nextCursor = $state<string>();
  let selectedId = $state<string>();
  let items = $state<MessageItem[]>([]);
  let draft = $state('');
  let listLoading = $state(true);
  let detailLoading = $state(false);
  let streamPending = $state(false);
  let createPending = $state(false);
  let loadMorePending = $state(false);
  let availability = $state<'checking' | 'available' | 'disabled'>('checking');
  let disabledReason = $state<string>();
  let problem = $state<Problem>();
  let streamNotice = $state('');
  let announcement = $state('');
  let deleteOpen = $state(false);
  let pendingDelete = $state<Conversation>();
  let generation = 0;
  let streamController: AbortController | undefined;
  let lifecycle = new AbortController();

  const selected = $derived(conversations.find(({ id }) => id === selectedId));
  const busy = $derived(streamPending || createPending || detailLoading);
  const sendDisabled = $derived(
    availability !== 'available' ||
      streamPending ||
      !selectedId ||
      !draft.trim(),
  );

  onMount(() => {
    void initialise();
    if (close)
      void tick().then(() =>
        root.querySelector<HTMLButtonElement>('[data-panel-close]')?.focus(),
      );
  });
  onDestroy(() => {
    lifecycle.abort();
    streamController?.abort();
    generation++;
  });

  async function initialise() {
    await Promise.all([checkAvailability(), loadConversations()]);
  }

  function asProblem(error: unknown, title: string): Problem {
    if (error instanceof ApiProblemError) return error.problem;
    return {
      title,
      detail: error instanceof Error ? error.message : undefined,
      recovery: 'Check the local GenAImage server and try again.',
    };
  }

  async function checkAvailability() {
    availability = 'checking';
    disabledReason = undefined;
    try {
      const health = await api.health({ signal: lifecycle.signal });
      if (lifecycle.signal.aborted) return;
      const reason = healthToDisabledReason(health, 'text');
      availability = reason ? 'disabled' : 'available';
      disabledReason = reason;
    } catch (error) {
      if (lifecycle.signal.aborted) return;
      const failure = asProblem(
        error,
        'Could not check assistant availability',
      );
      availability = 'disabled';
      disabledReason =
        problemToDisabledReason(
          failure,
          'The configured text model is unavailable for the assistant.',
        ) ?? 'Could not check the assistant. Check Settings and try again.';
      problem = failure;
    }
  }

  async function loadConversations(cursor?: string) {
    if (cursor) loadMorePending = true;
    else listLoading = true;
    try {
      const page = await api.listAssistantConversations(
        { cursor, limit: 20 },
        { signal: lifecycle.signal },
      );
      if (lifecycle.signal.aborted) return;
      conversations = cursor
        ? [...conversations, ...page.conversations]
        : page.conversations;
      nextCursor = page.next_cursor ?? undefined;
      if (!selectedId && conversations[0])
        await selectConversation(conversations[0].id);
    } catch (error) {
      if (lifecycle.signal.aborted) return;
      problem = asProblem(error, 'Could not load conversations');
    } finally {
      listLoading = false;
      loadMorePending = false;
    }
  }

  async function selectConversation(id: string) {
    if (id === selectedId && items.length) return;
    generation++;
    streamController?.abort();
    streamController = undefined;
    streamPending = false;
    selectedId = id;
    items = [];
    streamNotice = '';
    await reloadConversation(id);
  }

  async function reloadConversation(
    id: string,
    recovery?: { userMessageId: string; notice: string },
  ): Promise<boolean> {
    const current = ++generation;
    detailLoading = true;
    try {
      const detail = await api.getAssistantConversation(id, 500, {
        signal: lifecycle.signal,
      });
      if (
        current !== generation ||
        id !== selectedId ||
        lifecycle.signal.aborted
      )
        return false;
      const persisted = messageItems(detail);
      if (recovery) {
        const submittedIndex = persisted.findIndex(
          (item) =>
            item.kind === 'message' &&
            item.role === 'user' &&
            item.id === recovery.userMessageId,
        );
        if (submittedIndex === -1) return false;
        const nextUser = persisted.findIndex(
          (item, index) =>
            index > submittedIndex &&
            item.kind === 'message' &&
            item.role === 'user',
        );
        const end = nextUser === -1 ? persisted.length : nextUser;
        const terminalStatuses: Extract<
          MessageItem,
          { kind: 'message' }
        >['status'][] = [
          'complete',
          'awaiting_confirmation',
          'cancelled',
          'failed',
        ];
        const candidates = persisted
          .slice(submittedIndex + 1, end)
          .filter(
            (item): item is Extract<MessageItem, { kind: 'message' }> =>
              item.kind === 'message' && item.role === 'assistant',
          )
          .filter((item) => terminalStatuses.includes(item.status));
        const candidate = candidates[candidates.length - 1];
        if (!candidate) return false;
        candidate.notice = recovery.notice;
      }
      const persistedIds = new Set(persisted.map(({ id }) => id));
      const transient = items.filter(
        (item) =>
          item.conversationId === id &&
          item.kind !== 'message' &&
          !persistedIds.has(item.id),
      );
      items = [...persisted, ...transient];
      await scrollTranscript();
      return true;
    } catch (error) {
      if (
        lifecycle.signal.aborted ||
        current !== generation ||
        id !== selectedId
      )
        return false;
      problem = asProblem(error, 'Could not load this conversation');
      return false;
    } finally {
      if (current === generation) detailLoading = false;
    }
  }

  async function createConversation() {
    if (createPending || availability !== 'available') return;
    createPending = true;
    problem = undefined;
    try {
      const conversation = await api.createAssistantConversation({
        title: 'New conversation',
      });
      conversations = [conversation, ...conversations];
      await selectConversation(conversation.id);
      announcement = 'New conversation created.';
    } catch (error) {
      problem = asProblem(error, 'Could not create a conversation');
      const reason = problemToDisabledReason(problem);
      if (reason) {
        availability = 'disabled';
        disabledReason = reason;
      }
    } finally {
      createPending = false;
    }
  }

  function requestDelete(conversation: Conversation) {
    pendingDelete = conversation;
    deleteOpen = true;
  }

  async function deleteConversation() {
    const target = pendingDelete;
    if (!target) return;
    deleteOpen = false;
    try {
      await api.deleteAssistantConversation(target.id);
      conversations = conversations.filter(({ id }) => id !== target.id);
      if (selectedId === target.id) {
        selectedId = undefined;
        items = [];
        if (conversations[0]) await selectConversation(conversations[0].id);
      }
      announcement = 'Conversation deleted.';
    } catch (error) {
      problem = asProblem(error, 'Could not delete the conversation');
    } finally {
      pendingDelete = undefined;
    }
  }

  function conflictReason(error: unknown) {
    if (!(error instanceof ApiProblemError) || error.status !== 409) return;
    const text = `${error.detail ?? ''} ${error.title ?? ''}`.toLowerCase();
    return text.includes('confirmation')
      ? 'A confirmation is pending before another reply can be sent.'
      : 'A reply is already in progress for this conversation.';
  }

  async function send(event: SubmitEvent) {
    event.preventDefault();
    if (sendDisabled || !selectedId) return;
    const conversationId = selectedId;
    const content = draft.trim();
    const controller = new AbortController();
    const current = ++generation;
    streamController = controller;
    streamPending = true;
    draft = '';
    problem = undefined;
    streamNotice = '';
    const seq = items.reduce(
      (max, item) => (item.kind === 'message' ? Math.max(max, item.seq) : max),
      0,
    );
    const userId = `stream-user-${current}`;
    const assistantId = `stream-assistant-${current}`;
    items = [
      ...items,
      {
        kind: 'message',
        id: userId,
        conversationId,
        seq: seq + 1,
        role: 'user',
        text: content,
        status: 'complete',
      },
      {
        kind: 'message',
        id: assistantId,
        conversationId,
        seq: seq + 2,
        role: 'assistant',
        text: '',
        status: 'streaming',
      },
    ];
    let terminal = false;
    let cancelled = false;
    let protocolError = false;
    let definitiveRejection = false;
    let submittedUserMessageId: string | undefined;
    try {
      const stream = await api.sendAssistantMessage(
        conversationId,
        { content: [{ text: content }] },
        { signal: controller.signal },
      );
      for await (const frame of stream) {
        if (current !== generation || conversationId !== selectedId) break;
        const streamEvent = {
          ...frame.data,
          event: frame.event,
        } as AssistantStreamEvent;
        if (streamEvent.conversation_id !== conversationId) continue;
        if (streamEvent.event === 'message_started') {
          submittedUserMessageId ??= streamEvent.user_message_id;
        } else if (streamEvent.event === 'delta') {
          updateAssistant(assistantId, (item) => ({
            ...item,
            text: item.text + streamEvent.text,
          }));
          await scrollTranscript();
        } else if (streamEvent.event === 'tool_call') {
          items = [
            ...items,
            {
              kind: 'tool_call',
              id: streamEvent.tool_call.id,
              conversationId,
              toolCall: streamEvent.tool_call,
            },
          ];
        } else if (streamEvent.event === 'tool_result') {
          items = [
            ...items,
            {
              kind: 'tool_result',
              id: `${streamEvent.tool_call_id}:result`,
              conversationId,
              toolCallId: streamEvent.tool_call_id,
              tool: streamEvent.tool,
              ok: streamEvent.ok,
              result: streamEvent.result,
              error: streamEvent.error ?? undefined,
            },
          ];
        } else if (streamEvent.event === 'confirmation_required') {
          items = [
            ...items,
            {
              kind: 'confirmation_required',
              id: streamEvent.confirmation_id,
              conversationId,
              confirmation: streamEvent,
            },
          ];
        } else if (streamEvent.event === 'done') {
          if (streamEvent.status !== 'complete') {
            protocolError = true;
            streamNotice = `Protocol error: done reported ${streamEvent.status}.`;
          } else {
            terminal = true;
            updateAssistant(assistantId, (item) => ({
              ...item,
              status: 'complete',
              error: undefined,
            }));
            announcement = 'Assistant reply complete.';
          }
          break;
        } else if (streamEvent.event === 'awaiting_confirmation') {
          if (streamEvent.status !== 'awaiting_confirmation') {
            protocolError = true;
            streamNotice = `Protocol error: awaiting_confirmation reported ${streamEvent.status}.`;
          } else {
            terminal = true;
            updateAssistant(assistantId, (item) => ({
              ...item,
              status: 'awaiting_confirmation',
              error: undefined,
            }));
            streamNotice = 'Waiting for your confirmation';
            announcement = 'Waiting for your confirmation.';
          }
          break;
        } else if (streamEvent.event === 'error') {
          if (!['failed', 'cancelled'].includes(streamEvent.status)) {
            protocolError = true;
            streamNotice =
              `Protocol error: error reported ${streamEvent.status}. ${streamEvent.error ?? ''}`.trim();
          } else {
            terminal = true;
            const reason = streamEvent.error ?? 'The assistant reply failed.';
            updateAssistant(assistantId, (item) => ({
              ...item,
              status: streamEvent.status,
              error: reason,
            }));
            streamNotice = reason;
            announcement = reason;
          }
          break;
        }
      }
    } catch (error) {
      cancelled = controller.signal.aborted;
      if (current !== generation || conversationId !== selectedId) return;
      if (!cancelled) {
        const conflict = conflictReason(error);
        if (conflict) {
          definitiveRejection = true;
          streamNotice = conflict;
        } else if (
          error instanceof ApiProblemError ||
          error instanceof ApiHttpError
        ) {
          definitiveRejection = true;
          problem = asProblem(error, 'Could not send the message');
          streamNotice = error.message;
        }
      }
    } finally {
      if (current === generation && conversationId === selectedId) {
        streamPending = false;
        cancelled ||= controller.signal.aborted;
        if (streamController === controller) streamController = undefined;
        if (!terminal) {
          if (definitiveRejection) {
            updateAssistant(assistantId, (item) => ({
              ...item,
              status: 'failed',
              error: streamNotice,
            }));
            announcement = streamNotice;
          } else {
            const interruptedNotice = protocolError
              ? streamNotice
              : cancelled
                ? 'The reply was stopped before completion.'
                : streamNotice ||
                  'The response was interrupted before a terminal event arrived.';
            streamNotice = interruptedNotice;
            updateAssistant(assistantId, (item) => ({
              ...item,
              status: 'failed',
              error: interruptedNotice,
            }));
            announcement = interruptedNotice;
            if (!protocolError && !cancelled && submittedUserMessageId) {
              const recovered = await reloadConversation(conversationId, {
                userMessageId: submittedUserMessageId,
                notice: 'Connection interrupted — reloaded from the server',
              });
              if (recovered) {
                streamNotice =
                  'Connection interrupted — reloaded from the server';
                announcement = streamNotice;
              }
            }
          }
        }
      }
    }
  }

  function updateAssistant(
    id: string,
    update: (item: Extract<MessageItem, { kind: 'message' }>) => MessageItem,
  ) {
    items = items.map((item) =>
      item.kind === 'message' && item.id === id
        ? update(item as Extract<MessageItem, { kind: 'message' }>)
        : item,
    );
  }

  async function stop() {
    if (!streamPending || !streamController || !selectedId) return;
    const conversationId = selectedId;
    const cancel = api
      .cancelAssistantConversation(conversationId)
      .catch((error) => {
        problem = asProblem(error, 'Could not stop the reply');
      });
    streamController.abort();
    await cancel;
  }

  async function scrollTranscript() {
    await tick();
    if (transcript) transcript.scrollTop = transcript.scrollHeight;
  }

  function panelKeydown(event: KeyboardEvent) {
    if (
      event.key === 'Escape' &&
      close &&
      !event.defaultPrevented &&
      !document.querySelector('dialog[open]')
    ) {
      event.preventDefault();
      close();
    }
  }
</script>

<svelte:window onkeydown={panelKeydown} />

<section
  class:assistant-dock={presentation === 'dock'}
  class:dock-left={presentation === 'dock' && side === 'left'}
  class:assistant-full={presentation === 'full'}
  class="assistant-panel"
  aria-label="Assistant"
  bind:this={root}
>
  <header class="assistant-header">
    <div>
      <h2>Assistant</h2>
      {#if selected}<p>{selected.title}</p>{/if}
    </div>
    <div class="assistant-header-actions">
      {#if presentation === 'dock' && setSide}
        <Button
          aria-label={`Dock assistant on the ${side === 'right' ? 'left' : 'right'}`}
          onclick={() => setSide(side === 'right' ? 'left' : 'right')}
          >Dock {side === 'right' ? 'left' : 'right'}</Button
        >
      {/if}
      {#if close}<Button data-panel-close onclick={close}
          >Close assistant</Button
        >{/if}
    </div>
  </header>

  <div class="assistant-body">
    <aside class="conversation-list" aria-label="Conversations">
      <Button
        variant="primary"
        disabled={availability !== 'available' || createPending}
        onclick={createConversation}
        >{createPending ? 'Creating…' : 'New conversation'}</Button
      >
      {#if listLoading}<p role="status">Loading conversations…</p>{/if}
      <ul>
        {#each conversations as conversation (conversation.id)}
          <li class:current={conversation.id === selectedId}>
            <button
              type="button"
              aria-current={conversation.id === selectedId ? 'true' : undefined}
              onclick={() => selectConversation(conversation.id)}
              >{conversation.title}</button
            >
            <Button
              aria-label={`Delete ${conversation.title}`}
              disabled={busy}
              onclick={() => requestDelete(conversation)}>Delete</Button
            >
          </li>
        {/each}
      </ul>
      {#if !listLoading && !conversations.length}<p>
          No conversations yet.
        </p>{/if}
      {#if nextCursor}
        <Button
          disabled={loadMorePending}
          onclick={() => loadConversations(nextCursor)}
          >{loadMorePending ? 'Loading…' : 'Load more'}</Button
        >
      {/if}
    </aside>

    <div class="assistant-chat">
      {#if availability === 'checking'}
        <p role="status">Checking assistant availability…</p>
      {:else if disabledReason}
        <div class="disabled-reason">
          <p>Assistant is disabled. {disabledReason}</p>
          <p>Configure the text model in <a href="#/settings">Settings</a>.</p>
          <Button onclick={checkAvailability}>Check again</Button>
        </div>
      {/if}
      {#if problem}<ProblemDetails {problem} />{/if}
      {#if streamNotice}<p
          class:waiting={streamNotice.startsWith('Waiting')}
          class="stream-notice"
        >
          {streamNotice}
        </p>{/if}
      <div
        class="transcript"
        bind:this={transcript}
        aria-busy={detailLoading || streamPending}
      >
        {#if detailLoading}<p role="status">Loading conversation…</p>{/if}
        {#if !selectedId && !listLoading}
          <p>Select or create a conversation to begin.</p>
        {/if}
        {#each items as item (item.id)}
          {#if item.conversationId === selectedId}
            {#if item.kind === 'message'}
              <article
                class:assistant-message={item.role === 'assistant'}
                class:user-message={item.role === 'user'}
              >
                <p class="message-role">
                  {item.role === 'user' ? 'You' : 'Assistant'}
                </p>
                {#if item.text}<Markdown
                    text={item.text}
                  />{:else if item.status === 'streaming'}<span
                    aria-hidden="true">…</span
                  >{/if}
                {#if item.error}
                  <p class="message-state error-text">{item.error}</p>
                {:else if item.status === 'failed' || item.status === 'cancelled'}
                  <p class="message-state error-text">
                    The reply did not complete.
                  </p>
                {:else if item.status === 'awaiting_confirmation'}
                  <p class="message-state">Waiting for your confirmation</p>
                {/if}
                {#if item.notice}<p class="message-state">{item.notice}</p>{/if}
              </article>
            {:else if item.kind === 'tool_call'}
              <ToolCallCard
                toolCall={item.toolCall}
                conversationId={item.conversationId}
              />
            {:else if item.kind === 'tool_result'}
              <ToolResultCard
                tool={item.tool}
                ok={item.ok}
                conversationId={item.conversationId}
                toolCallId={item.toolCallId}
                result={item.result}
                error={item.error}
              />
            {:else}
              <ConfirmationCard
                confirmation={item.confirmation}
                conversationId={item.conversationId}
              />
            {/if}
          {/if}
        {/each}
      </div>
      <div class="visually-hidden" aria-live="polite" aria-atomic="true">
        {announcement}
      </div>
      <form class="composer" onsubmit={send}>
        <label for="assistant-message">Message</label>
        <textarea
          id="assistant-message"
          rows="3"
          class="ui-control"
          bind:value={draft}
          disabled={availability !== 'available' ||
            !selectedId ||
            streamPending}
          placeholder="Ask the assistant…"
        ></textarea>
        <div class="composer-actions">
          {#if streamPending}<Button onclick={stop}>Stop</Button>{/if}
          <Button type="submit" variant="primary" disabled={sendDisabled}
            >Send</Button
          >
        </div>
      </form>
    </div>
  </div>
</section>

<Dialog bind:open={deleteOpen} title="Delete conversation">
  <p>This removes the conversation and its messages. This cannot be undone.</p>
  <div class="dialog-actions">
    <Button onclick={() => (deleteOpen = false)}>Keep conversation</Button>
    <Button variant="primary" onclick={deleteConversation}
      >Delete conversation</Button
    >
  </div>
</Dialog>

<style>
  .assistant-panel {
    display: grid;
    min-inline-size: 0;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-panel);
    box-shadow: var(--shadow-raised);
    overflow: hidden;
  }
  .assistant-dock {
    position: fixed;
    z-index: 4;
    inset-block: var(--space-4);
    inset-inline-end: var(--space-4);
    inline-size: min(34rem, calc(100vw - 2 * var(--space-4)));
  }
  .assistant-dock.dock-left {
    inset-inline-end: auto;
    inset-inline-start: var(--space-4);
  }
  .assistant-full {
    min-block-size: 70vh;
  }
  .assistant-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border-block-end: 1px solid var(--border);
  }
  .assistant-header h2 {
    font-size: 1.25rem;
  }
  .assistant-header p {
    color: var(--muted);
    font-size: var(--text-small);
  }
  .assistant-header-actions,
  .composer-actions,
  .dialog-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }
  .assistant-body {
    display: grid;
    grid-template-columns: minmax(10rem, 0.7fr) minmax(0, 2fr);
    min-block-size: 0;
  }
  .conversation-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-block-size: 0;
    padding: var(--space-3);
    border-inline-end: 1px solid var(--border);
    overflow: auto;
  }
  .conversation-list ul {
    display: grid;
    gap: var(--space-2);
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .conversation-list li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-1);
  }
  .conversation-list li.current {
    outline: 1px solid var(--accent);
    outline-offset: 1px;
    border-radius: var(--radius-control);
  }
  .conversation-list li > button:first-child {
    min-block-size: var(--tap-target);
    min-inline-size: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: start;
    color: var(--ink);
    background: var(--surface);
    border: 0;
    cursor: pointer;
  }
  .assistant-chat {
    display: grid;
    grid-template-rows: auto minmax(8rem, 1fr) auto;
    min-block-size: 0;
    gap: var(--space-3);
    padding: var(--space-3);
  }
  .disabled-reason,
  .stream-notice {
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
  }
  .disabled-reason {
    background: var(--subtle);
  }
  .transcript {
    display: grid;
    align-content: start;
    gap: var(--space-3);
    overflow: auto;
    min-block-size: 8rem;
    padding-inline-end: var(--space-1);
  }
  article,
  :global(.tool-placeholder),
  :global(.confirmation-placeholder) {
    padding: var(--space-3);
    border-radius: var(--radius-control);
  }
  .user-message {
    margin-inline-start: 12%;
    background: var(--subtle);
  }
  .assistant-message {
    margin-inline-end: 8%;
    border: 1px solid var(--border);
  }
  .message-role {
    font-weight: var(--weight-strong);
    margin-block-end: var(--space-2);
  }
  .message-state {
    margin-block-start: var(--space-2);
    font-size: var(--text-small);
  }
  .waiting {
    border-color: var(--accent);
  }
  .error-text {
    color: var(--danger);
  }
  :global(.tool-placeholder),
  :global(.confirmation-placeholder) {
    border: 1px dashed var(--border);
    color: var(--muted);
  }
  .composer {
    display: grid;
    gap: var(--space-2);
  }
  .composer label {
    font-weight: var(--weight-strong);
  }
  .composer textarea {
    resize: vertical;
    min-block-size: 5.5rem;
  }
  @media (max-width: 48rem) {
    .assistant-dock {
      inset: var(--space-2);
      inline-size: auto;
    }
    .assistant-body {
      grid-template-columns: 1fr;
    }
    .conversation-list {
      max-block-size: 12rem;
      border-inline-end: 0;
      border-block-end: 1px solid var(--border);
    }
  }
</style>
