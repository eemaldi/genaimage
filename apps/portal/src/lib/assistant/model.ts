import type { components } from '../api/schema';

export type Conversation =
  components['schemas']['AssistantConversationResponse'];
export type ConversationDetail =
  components['schemas']['AssistantConversationDetailResponse'];
export type StoredMessage = components['schemas']['AssistantMessageResponse'];
export type ToolCall = components['schemas']['AssistantToolCall'];
export type Confirmation =
  components['schemas']['AssistantConfirmationRequiredEvent'] & {
    invocation_result?: unknown;
    invocation_error?: string | null;
    validation_error?: string | null;
  };

export type MessageItem =
  | {
      kind: 'message';
      id: string;
      conversationId: string;
      seq: number;
      role: 'user' | 'assistant';
      text: string;
      model?: string;
      status: components['schemas']['AssistantMessageStatus'] | 'streaming';
      error?: string;
      notice?: string;
      createdAt?: string;
    }
  | {
      kind: 'tool_call';
      id: string;
      conversationId: string;
      toolCall: ToolCall;
    }
  | {
      kind: 'tool_result';
      id: string;
      conversationId: string;
      toolCallId: string;
      tool: string;
      ok?: boolean;
      result?: unknown;
      error?: string;
    }
  | {
      kind: 'confirmation_required';
      id: string;
      conversationId: string;
      confirmation: Confirmation;
    };

export function messageItems(
  detail: ConversationDetail,
  audit: components['schemas']['AssistantToolAuditResponse']['tool_calls'] = [],
): MessageItem[] {
  const items: MessageItem[] = [];
  const toolNames = new Map<string, string>();
  const auditByCall = new Map<string, (typeof audit)[number]>();
  for (const record of audit) {
    if (!auditByCall.has(record.tool_call_id))
      auditByCall.set(record.tool_call_id, record);
  }
  const confirmationStatuses = new Set<Confirmation['status']>([
    'pending',
    'invoking',
    'succeeded',
    'failed',
    'rejected',
    'outcome_unknown',
    'expired',
  ]);

  for (const message of detail.messages) {
    const content = message.content;
    if ('user' in content) {
      const text = content.user
        .map((part) => ('text' in part ? part.text : '[Image attachment]'))
        .join('\n');
      items.push({
        kind: 'message',
        id: message.id,
        conversationId: message.conversation_id,
        seq: message.seq,
        role: 'user',
        text,
        status: message.status,
        error: message.error ?? undefined,
        createdAt: message.created_at,
      });
    } else if ('assistant' in content) {
      items.push({
        kind: 'message',
        id: message.id,
        conversationId: message.conversation_id,
        seq: message.seq,
        role: 'assistant',
        text: content.assistant.text,
        model: message.model ?? undefined,
        status: message.status,
        error: message.error ?? undefined,
        createdAt: message.created_at,
      });
      for (const toolCall of content.assistant.tool_calls) {
        toolNames.set(toolCall.id, toolCall.name);
        items.push({
          kind: 'tool_call',
          id: `${message.id}:${toolCall.id}`,
          conversationId: message.conversation_id,
          toolCall,
        });
        const record = auditByCall.get(toolCall.id);
        const status = record?.confirmation_status as Confirmation['status'];
        if (record?.confirmation_id && confirmationStatuses.has(status)) {
          items.push({
            kind: 'confirmation_required',
            id: record.confirmation_id,
            conversationId: message.conversation_id,
            confirmation: {
              confirmation_id: record.confirmation_id,
              status,
              conversation_id: message.conversation_id,
              tool_call_id: record.tool_call_id,
              tool: record.tool,
              arguments:
                status === 'pending'
                  ? record.proposed_arguments
                  : (record.effective_arguments ?? record.proposed_arguments),
              json_schema: {},
              preview: undefined,
              preview_token: '',
              invocation_result: record.invocation_result,
              invocation_error: record.invocation_error,
              validation_error: record.validation_error,
            },
          });
        }
      }
    } else if ('tool' in content) {
      const persisted =
        content.tool.result &&
        typeof content.tool.result === 'object' &&
        !Array.isArray(content.tool.result)
          ? (content.tool.result as Record<string, unknown>)
          : undefined;
      const ok = typeof persisted?.ok === 'boolean' ? persisted.ok : undefined;
      const nestedError =
        typeof persisted?.error === 'string' ? persisted.error : undefined;
      items.push({
        kind: 'tool_result',
        id: message.id,
        conversationId: message.conversation_id,
        toolCallId: content.tool.call_id,
        tool:
          toolNames.get(content.tool.call_id) ??
          auditByCall.get(content.tool.call_id)?.tool ??
          content.tool.call_id,
        ok,
        result: persisted?.result,
        error:
          nestedError ??
          (ok === undefined
            ? 'The persisted tool outcome is malformed or unavailable.'
            : undefined),
      });
    }
  }
  return items;
}
