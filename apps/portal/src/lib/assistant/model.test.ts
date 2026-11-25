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
