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
import { api, ApiProblemError } from '../api/client';
import type { components } from '../api/schema';
import { mockNativeDialog } from '../../test/dialog';
import AssistantPanel from './AssistantPanel.svelte';

mockNativeDialog();

type Conversation = components['schemas']['AssistantConversationResponse'];
type Detail = components['schemas']['AssistantConversationDetailResponse'];

const available = {
  self: 'ok' as const,
  image_backend: 'ok' as const,
  llm: 'ok' as const,
  llm_text: { status: 'ok' as const, model: 'local' },
};
const conversation = (id: string, title = id): Conversation => ({
  id,
  title,
  model: 'local',
  system_prompt: '',
  system_prompt_sha256: 'hash',
  system_prompt_template: 'assistant',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
});
const detail = (id: string): Detail => ({
  ...conversation(id),
  messages: [],
});

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => (resolve = done));
  return { promise, resolve };
}

beforeEach(() => {
  vi.spyOn(api, 'health').mockResolvedValue(available);
  vi.spyOn(api, 'listAssistantConversations').mockResolvedValue({
    conversations: [conversation('one')],
    next_cursor: null,
  });
  vi.spyOn(api, 'getAssistantConversation').mockImplementation(async (id) =>
    detail(id),
  );
  vi.spyOn(api, 'createAssistantConversation').mockImplementation(async () =>
    conversation('new', 'New conversation'),
  );
  vi.spyOn(api, 'deleteAssistantConversation').mockResolvedValue(undefined);
  vi.spyOn(api, 'cancelAssistantConversation').mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function startWithSelection() {
  render(AssistantPanel, { presentation: 'full' });
  await screen.findByRole('button', { name: 'one' });
  await waitFor(() =>
    expect(screen.getByLabelText('Message').hasAttribute('disabled')).toBe(
      false,
    ),
  );
  return screen.getByLabelText('Message');
}

it('renders streamed deltas and completes only from done', async () => {
  const terminal = deferred();
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'delta',
        data: { conversation_id: 'one', user_message_id: 'u', text: 'Hello' },
      };
      await terminal.promise;
      yield {
        event: 'done',
        data: {
          conversation_id: 'one',
          user_message_id: 'u',
          message_id: 'a',
          model: 'local',
          status: 'complete' as const,
        },
      };
    })(),
  );
  const user = userEvent.setup();
  const textarea = await startWithSelection();
  await user.type(textarea, 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(await screen.findByText('Hello')).toBeTruthy();
  expect(screen.queryByText('Assistant reply complete.')).toBeNull();
  terminal.resolve();
  await waitFor(() => expect(screen.getAllByText('Hello')).not.toHaveLength(0));
  expect(api.getAssistantConversation).toHaveBeenCalledTimes(1);
});

it.each([
  ['done', 'failed', undefined],
  ['awaiting_confirmation', 'complete', undefined],
  ['error', 'complete', 'Keep this reason.'],
] as const)(
  'treats contradictory %s payload status as an interrupted protocol error',
  async (event, status, error) => {
    vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
      (async function* () {
        yield {
          event,
          data: {
            conversation_id: 'one',
            user_message_id: 'u',
            message_id: 'a',
            model: 'local',
            status,
            error,
          },
        } as never;
      })(),
    );
    const user = userEvent.setup();
    await user.type(await startWithSelection(), 'Hi');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(
      (await screen.findAllByText(new RegExp(`Protocol error: ${event}`)))[0],
    ).toBeTruthy();
    expect(screen.queryByText('Assistant reply complete.')).toBeNull();
    if (error)
      expect(screen.getAllByText(/Keep this reason/)).not.toHaveLength(0);
    expect(api.getAssistantConversation).toHaveBeenCalledTimes(1);
  },
);

it('keeps an error reason visible for the error terminal event', async () => {
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'error',
        data: {
          conversation_id: 'one',
          user_message_id: 'u',
          message_id: 'a',
          model: 'local',
          status: 'cancelled',
          error: 'Stopped by the configured provider.',
        },
      } as never;
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(
    screen.getAllByText('Stopped by the configured provider.'),
  ).not.toHaveLength(0);
});

it('Stop cancels and aborts the active fetch', async () => {
  let signal: AbortSignal | undefined;
  vi.spyOn(api, 'sendAssistantMessage').mockImplementation(
    async (_id, _body, options) => {
      signal = options?.signal;
      return (async function* () {
        await new Promise<void>((resolve) =>
          signal?.addEventListener('abort', () => resolve(), { once: true }),
        );
        yield* [] as never[];
      })();
    },
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Stop this');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await user.click(await screen.findByRole('button', { name: 'Stop' }));
  expect(api.cancelAssistantConversation).toHaveBeenCalledWith('one');
  expect(signal?.aborted).toBe(true);
  expect(
    (await screen.findAllByText('The reply was stopped before completion.'))[0],
  ).toBeTruthy();
});

it.each([
  [
    'awaiting_confirmation',
    {
      conversation_id: 'one',
      user_message_id: 'u',
      message_id: 'a',
      confirmation_id: 'c',
      model: 'local',
      status: 'awaiting_confirmation' as const,
    },
    'Waiting for your confirmation',
  ],
  [
    'error',
    {
      conversation_id: 'one',
      user_message_id: 'u',
      message_id: 'a',
      model: 'local',
      status: 'failed' as const,
      error: 'Provider rejected the request.',
    },
    'Provider rejected the request.',
  ],
] as const)('shows %s as not completed', async (_event, data, message) => {
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield { event: _event, data } as never;
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(await screen.findAllByText(message)).not.toHaveLength(0);
  expect(screen.queryByText('Assistant reply complete.')).toBeNull();
});

it('shows an unterminated stream as interrupted rather than complete', async () => {
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'delta',
        data: { conversation_id: 'one', user_message_id: 'u', text: 'partial' },
      };
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(
    await screen.findAllByText(
      'The response was interrupted before a terminal event arrived.',
    ),
  ).toBeTruthy();
  expect(screen.queryByText('Assistant reply complete.')).toBeNull();
});

it('marks a completed server reply as reloaded after an interrupted connection', async () => {
  const persisted: Detail = {
    ...detail('one'),
    messages: [
      {
        id: 'persisted-assistant',
        conversation_id: 'one',
        created_at: '2026-01-01T00:00:01Z',
        seq: 2,
        role: 'assistant',
        model: 'local',
        status: 'complete',
        content: {
          assistant: { text: 'Server-confirmed answer.', tool_calls: [] },
        },
      },
    ],
  };
  vi.mocked(api.getAssistantConversation)
    .mockResolvedValueOnce(detail('one'))
    .mockResolvedValueOnce(persisted);
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'delta',
        data: {
          conversation_id: 'one',
          user_message_id: 'persisted-user',
          text: 'Partial',
        },
      };
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(await screen.findByText('Server-confirmed answer.')).toBeTruthy();
  expect(
    screen.getAllByText('Connection interrupted — reloaded from the server'),
  ).not.toHaveLength(0);
  expect(screen.queryByText('Partial')).toBeNull();
});

it('keeps the local interrupted state when authoritative reload fails', async () => {
  vi.mocked(api.getAssistantConversation)
    .mockResolvedValueOnce(detail('one'))
    .mockRejectedValueOnce(new Error('Reload unavailable.'));
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'delta',
        data: {
          conversation_id: 'one',
          user_message_id: 'u',
          text: 'Local partial answer.',
        },
      };
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(await screen.findByText('Local partial answer.')).toBeTruthy();
  expect(
    (
      await screen.findAllByText(
        'The response was interrupted before a terminal event arrived.',
      )
    )[0],
  ).toBeTruthy();
  expect(
    await screen.findByText('Could not load this conversation'),
  ).toBeTruthy();
});

it('keeps the local interrupted state when the persisted reply is missing', async () => {
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      yield {
        event: 'delta',
        data: { conversation_id: 'one', user_message_id: 'u', text: 'Unsaved' },
      };
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(await screen.findByText('Unsaved')).toBeTruthy();
  expect(
    (
      await screen.findAllByText(
        'The response was interrupted before a terminal event arrived.',
      )
    )[0],
  ).toBeTruthy();
});

it('ignores a stale stream after switching conversations', async () => {
  vi.mocked(api.listAssistantConversations).mockResolvedValue({
    conversations: [conversation('one'), conversation('two')],
    next_cursor: null,
  });
  const release = deferred();
  vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      await release.promise;
      yield {
        event: 'delta',
        data: {
          conversation_id: 'one',
          user_message_id: 'u',
          text: 'stale token',
        },
      };
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  await user.click(screen.getByRole('button', { name: 'two' }));
  release.resolve();
  await waitFor(() => expect(screen.queryByText('stale token')).toBeNull());
});

it('does not send twice while a reply is in flight', async () => {
  const terminal = deferred();
  const send = vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
    (async function* () {
      await terminal.promise;
      yield* [] as never[];
    })(),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  const form = screen.getByLabelText('Message').closest('form')!;
  await fireEvent.submit(form);
  expect(send).toHaveBeenCalledTimes(1);
  terminal.resolve();
});

it('explains a 409 without resending', async () => {
  vi.spyOn(api, 'sendAssistantMessage').mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Conflict',
      status: 409,
      detail: 'A confirmation is pending.',
      recovery: 'Confirm first.',
    }),
  );
  const user = userEvent.setup();
  await user.type(await startWithSelection(), 'Hi');
  await user.click(screen.getByRole('button', { name: 'Send' }));
  expect(
    await screen.findAllByText(
      'A confirmation is pending before another reply can be sent.',
    ),
  ).toBeTruthy();
});

it('paginates the bounded conversation list', async () => {
  vi.mocked(api.listAssistantConversations)
    .mockResolvedValueOnce({
      conversations: [conversation('one')],
      next_cursor: 'next',
    })
    .mockResolvedValueOnce({
      conversations: [conversation('two')],
      next_cursor: null,
    });
  const user = userEvent.setup();
  render(AssistantPanel, { presentation: 'full' });
  await user.click(await screen.findByRole('button', { name: 'Load more' }));
  expect(api.listAssistantConversations).toHaveBeenNthCalledWith(
    2,
    { cursor: 'next', limit: 20 },
    expect.anything(),
  );
  expect(await screen.findByRole('button', { name: 'two' })).toBeTruthy();
});

it('only deletes after confirmation', async () => {
  const user = userEvent.setup();
  render(AssistantPanel, { presentation: 'full' });
  await user.click(await screen.findByRole('button', { name: 'Delete one' }));
  const dialog = screen.getByRole('dialog', { name: 'Delete conversation' });
  expect(api.deleteAssistantConversation).not.toHaveBeenCalled();
  await user.click(
    within(dialog).getByRole('button', { name: 'Delete conversation' }),
  );
  expect(api.deleteAssistantConversation).toHaveBeenCalledWith('one');
});

it('shows the disabled reason and Settings link', async () => {
  vi.mocked(api.health).mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'not_configured',
    llm_text: { status: 'not_configured', reason: 'Choose a text model.' },
  });
  render(AssistantPanel, { presentation: 'full' });
  expect(
    (await screen.findByText(/Assistant is disabled/)).textContent,
  ).toContain('Choose a text model.');
  expect(screen.getByRole('link', { name: 'Settings' })).toBeTruthy();
  expect(
    (screen.getByLabelText('Message') as HTMLTextAreaElement).disabled,
  ).toBe(true);
});

describe('placeholder extension items', () => {
  it('renders tool and confirmation events as replaceable items', async () => {
    vi.spyOn(api, 'sendAssistantMessage').mockResolvedValue(
      (async function* () {
        yield {
          event: 'tool_call',
          data: {
            conversation_id: 'one',
            user_message_id: 'u',
            tool_call: { id: 't', name: 'submit_run', arguments: {} },
          },
        };
        yield {
          event: 'confirmation_required',
          data: {
            conversation_id: 'one',
            confirmation_id: 'c',
            tool_call_id: 't',
            tool: 'submit_run',
            arguments: {},
            json_schema: {},
            preview: {},
            preview_token: 'token',
            status: 'pending' as const,
          },
        };
      })(),
    );
    const user = userEvent.setup();
    await user.type(await startWithSelection(), 'Run it');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByText(/A tool call happened/)).toBeTruthy();
    expect(await screen.findByText(/Confirmation required/)).toBeTruthy();
  });
});
