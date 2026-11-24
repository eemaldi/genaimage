export type SseFrame<T = unknown> = {
  event: string;
  data: T;
  id?: string;
};

/** Parse JSON SSE frames from a fetch response, including POST responses. */
export async function* readSseJson<T>(
  response: Response,
  signal?: AbortSignal,
): AsyncGenerator<SseFrame<T>> {
  if (!response.body)
    throw new Error('The server did not return an SSE response body.');
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  function parseRecord(record: string): SseFrame<T> | undefined {
    let event = 'message';
    let id: string | undefined;
    const data: string[] = [];
    for (const line of record.split(/\r?\n/)) {
      if (!line || line.startsWith(':')) continue;
      const colon = line.indexOf(':');
      const field = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? '' : line.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      if (field === 'event') event = value;
      else if (field === 'data') data.push(value);
      else if (field === 'id' && !value.includes('\0')) id = value;
    }
    if (!data.length) return undefined;
    return { event, data: JSON.parse(data.join('\n')) as T, id };
  }

  try {
    while (true) {
      signal?.throwIfAborted();
      const chunk = await reader.read();
      if (chunk.done) break;
      buffer += decoder.decode(chunk.value, { stream: true });
      let separator = buffer.match(/\r?\n\r?\n/);
      while (separator?.index !== undefined) {
        const record = buffer.slice(0, separator.index);
        buffer = buffer.slice(separator.index + separator[0].length);
        const frame = parseRecord(record);
        if (frame) yield frame;
        separator = buffer.match(/\r?\n\r?\n/);
      }
    }
    buffer += decoder.decode();
    if (buffer) {
      const frame = parseRecord(buffer);
      if (frame) yield frame;
    }
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}
