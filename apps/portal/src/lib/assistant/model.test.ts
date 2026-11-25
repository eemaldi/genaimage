import { expect, it } from 'vitest';
import type { components } from '../api/schema';
import { messageItems } from './model';

it('reconstructs persisted confirmation states from the tool audit', () => {
  const detail: components['schemas']['AssistantConversationDetailResponse'] = {
    id: 'conversation-1',
    title: 'Conversation',
    model: 'local',
    system_prompt: '',
    system_prompt_sha256: 'hash',
    system_prompt_template: 'assistant',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    messages: [
      {
        id: 'message-1',
        conversation_id: 'conversation-1',
        seq: 1,
        role: 'assistant',
        model: 'local',
        status: 'awaiting_confirmation',
        created_at: '2026-01-01T00:00:00Z',
        content: {
          assistant: {
            text: '',
            tool_calls: [
              { id: 'tool-1', name: 'submit_run', arguments: { seed: 1 } },
            ],
          },
        },
      },
    ],
  };
  for (const status of [
    'pending',
    'invoking',
    'succeeded',
    'failed',
    'outcome_unknown',
    'rejected',
    'expired',
  ] as const) {
    const items = messageItems(detail, [
      {
        id: 1,
        conversation_id: 'conversation-1',
        confirmation_id: 'confirmation-1',
        confirmation_status: status,
        tool_call_id: 'tool-1',
        tool: 'submit_run',
        proposed_arguments: { seed: 1 },
        validation_outcome: 'valid',
        decision: 'confirm',
        invocation_result:
          status === 'succeeded'
            ? { run_id: 'run-1', status: 'queued' }
            : undefined,
        invocation_error: status === 'failed' ? 'Reason' : undefined,
        created_at: '2026-01-01T00:00:00Z',
      },
    ]);
    const restored = items.find(
      (item) => item.kind === 'confirmation_required',
    );
    expect(restored?.confirmation.status).toBe(status);
  }
});

it('chooses the newest audit state and keeps proposed arguments while pending', () => {
  const detail: components['schemas']['AssistantConversationDetailResponse'] = {
    id: 'conversation-1',
    title: 'Conversation',
    model: 'local',
    system_prompt: '',
    system_prompt_sha256: 'hash',
    system_prompt_template: 'assistant',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    messages: [
      {
        id: 'message-1',
        conversation_id: 'conversation-1',
        seq: 1,
        role: 'assistant',
        model: 'local',
        status: 'awaiting_confirmation',
        created_at: '2026-01-01T00:00:00Z',
        content: {
          assistant: {
            text: '',
            tool_calls: [
              { id: 'tool-1', name: 'submit_run', arguments: { seed: 1 } },
            ],
          },
        },
      },
    ],
  };
  const base: components['schemas']['AssistantToolAuditResponse']['tool_calls'][number] =
    {
      id: 1,
      conversation_id: 'conversation-1',
      confirmation_id: 'confirmation-1',
      confirmation_status: 'pending',
      tool_call_id: 'tool-1',
      tool: 'submit_run',
      proposed_arguments: { seed: 1 },
      validation_outcome: 'valid',
      decision: 'pending',
      effective_arguments: undefined,
      created_at: '2026-01-01T00:00:00Z',
    };

  const pending = messageItems(detail, [
    {
      ...base,
      id: 2,
      decision: 'edit_invalid',
      effective_arguments: { seed: -1 },
      validation_error: 'seed: must be positive',
    },
    base,
  ]).find((item) => item.kind === 'confirmation_required');
  expect(pending?.confirmation.arguments).toEqual({ seed: 1 });
  expect(pending?.confirmation.status).toBe('pending');

  const invoking = messageItems(detail, [
    {
      ...base,
      id: 3,
      confirmation_status: 'invoking',
      decision: 'invocation_started',
      effective_arguments: { seed: 2 },
    },
    base,
  ]).find((item) => item.kind === 'confirmation_required');
  expect(invoking?.confirmation.status).toBe('invoking');
  expect(invoking?.confirmation.arguments).toEqual({ seed: 2 });

  const settled = messageItems(detail, [
    {
      ...base,
      id: 4,
      confirmation_status: 'succeeded',
      decision: 'invocation_succeeded',
      effective_arguments: { seed: 3 },
      invocation_result: { run_id: 'run-new', status: 'queued' },
      invocation_ok: true,
    },
    {
      ...base,
      id: 3,
      confirmation_status: 'invoking',
      decision: 'invocation_started',
      effective_arguments: { seed: 2 },
    },
    base,
  ]).find((item) => item.kind === 'confirmation_required');
  expect(settled?.confirmation.status).toBe('succeeded');
  expect(settled?.confirmation.arguments).toEqual({ seed: 3 });
  expect(settled?.confirmation.invocation_result).toEqual({
    run_id: 'run-new',
    status: 'queued',
  });
});

it('unwraps persisted tool outcomes without trusting the message error', () => {
  const detail = (result: unknown, error: string | null = 'outer error') => ({
    id: 'conversation-1',
    title: 'Conversation',
    model: 'local',
    system_prompt: '',
    system_prompt_sha256: 'hash',
    system_prompt_template: 'assistant',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    messages: [
      {
        id: 'tool-message',
        conversation_id: 'conversation-1',
        seq: 2,
        role: 'tool' as const,
        status: 'complete' as const,
        created_at: '2026-01-01T00:00:00Z',
        error,
        tool_call_id: 'tool-1',
        content: { tool: { call_id: 'tool-1', result } },
      },
    ],
  });

  const failed = messageItems(
    detail({ ok: false, error: 'persisted failure' }, 'stale outer error'),
  ).at(-1);
  expect(failed).toMatchObject({
    kind: 'tool_result',
    ok: false,
    result: undefined,
    error: 'persisted failure',
  });

  const succeeded = messageItems(
    detail({ ok: true, result: { run_id: 'run-1', status: 'queued' } }, null),
  ).at(-1);
  expect(succeeded).toMatchObject({
    kind: 'tool_result',
    ok: true,
    result: { run_id: 'run-1', status: 'queued' },
    error: undefined,
  });

  const unknown = messageItems(detail({ result: {} })).at(-1);
  expect(unknown).toMatchObject({
    kind: 'tool_result',
    ok: undefined,
    error: expect.stringMatching(/malformed or unavailable/),
  });
});
