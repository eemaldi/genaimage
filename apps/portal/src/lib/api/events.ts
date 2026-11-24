import { writable } from 'svelte/store';
import type { Readable } from 'svelte/store';
import type { components, operations } from './schema';
import { apiUrl } from './url';

export type EventEnvelope = components['schemas']['EventEnvelope'];
export type EventType = EventEnvelope['type'];
export type EventOf<T extends EventType> = Extract<EventEnvelope, { type: T }>;
export type ResyncEvent = components['schemas']['ResyncEvent'];
export type ResyncReason =
  ResyncEvent | { reason: 'cursor_unavailable' | 'invalid_frame' };
export type ResyncSignal = { epoch: number; reason: ResyncReason };
export type ConnectionStatus =
  'connecting' | 'open' | 'reconnecting' | 'closed';
export type EventSourceConstructor = new (
  url: string,
) => Pick<EventTarget, 'addEventListener'> & { close(): void };

const eventNames = {
  run_queued: true,
  run_started: true,
  run_progress: true,
  run_output: true,
  run_completed: true,
  run_failed: true,
  run_cancelled: true,
  run_unknown_submission: true,
  backend_health_changed: true,
  settings_changed: true,
} satisfies Record<EventType, true>;
const initialDelay = 250;
const maxDelay = 10_000;

function parse(message: MessageEvent<string>): unknown {
  try {
    return JSON.parse(message.data);
  } catch {
    return null;
  }
}

/** Pages should share `events`; the factory allows tests to inject EventSource. */
export function createEventStore(EventSourceImpl?: EventSourceConstructor) {
  const status = writable<ConnectionStatus>('closed');
  const resync = writable<ResyncSignal | null>(null);
  const listeners = new Set<(event: EventEnvelope) => void>();
  let subscribers = 0;
  let epoch = 0;
  let source: InstanceType<EventSourceConstructor> | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let delay = initialDelay;
  let lastId: string | undefined;
  let busId: string | undefined;
  let pendingResync: ResyncReason | undefined;

  function close() {
    const stream = source;
    source = undefined;
    stream?.close();
  }

  function reconnect() {
    close(); // Stop native reconnects; a new URL carries the updated cursor.
    const timeout = delay;
    delay = Math.min(delay * 2, maxDelay);
    timer = setTimeout(() => {
      timer = undefined;
      connect();
    }, timeout);
    status.set('reconnecting');
  }

  function resetCursor(reason: ResyncReason) {
    lastId = busId = undefined;
    pendingResync = reason;
    reconnect();
  }

  function connect() {
    const query: operations['events_events']['parameters']['query'] = {
      last_event_id: lastId,
    };
    let stream: InstanceType<EventSourceConstructor>;
    try {
      const Source = EventSourceImpl ?? EventSource;
      stream = new Source(apiUrl('/api/v1/events', undefined, query));
    } catch {
      reconnect();
      return;
    }
    source = stream;
    stream.addEventListener('open', () => {
      if (source !== stream) return;
      delay = initialDelay;
      status.set('open');
      if (source === stream && pendingResync) {
        const reason = pendingResync;
        pendingResync = undefined;
        // The live subscription is established before pages refetch snapshots.
        resync.set({ epoch: ++epoch, reason });
      }
    });
    stream.addEventListener('error', () => {
      if (source !== stream) return;
      if (!lastId) pendingResync ??= { reason: 'cursor_unavailable' };
      reconnect();
    });
    stream.addEventListener('resync', (raw) => {
      if (source !== stream) return;
      const data = parse(raw as MessageEvent<string>);
      resetCursor(
        data && typeof data === 'object' && 'reason' in data
          ? (data as ResyncEvent)
          : { reason: 'invalid_frame' },
      );
    });
    for (const name of Object.keys(eventNames)) {
      stream.addEventListener(name, (raw) => {
        if (source !== stream) return;
        const message = raw as MessageEvent<string>;
        const event = parse(message) as EventEnvelope | null;
        if (
          event?.type !== name ||
          typeof event.bus_id !== 'string' ||
          !/^\d+$/.test(message.lastEventId)
        ) {
          resetCursor({ reason: 'invalid_frame' });
          return;
        }
        if (busId && event.bus_id !== busId) {
          resetCursor({
            reason: 'bus_changed',
            requested: busId,
            current: event.bus_id,
          });
          return;
        }
        busId = event.bus_id;
        // Use the wire ID, not the JSON number, to preserve all u64 digits.
        lastId = message.lastEventId;
        for (const listener of listeners) listener(event);
      });
    }
  }

  function retain(): () => void {
    if (++subscribers === 1) {
      status.set('connecting');
      connect();
    }
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      if (--subscribers !== 0) return;
      clearTimeout(timer);
      timer = undefined;
      close();
      delay = initialDelay;
      lastId = busId = pendingResync = undefined;
      resync.set(null);
      status.set('closed');
    };
  }

  function shared<T>(store: Readable<T>): Readable<T> {
    return {
      subscribe(run, invalidate) {
        const release = retain();
        const unsubscribe = store.subscribe(run, invalidate);
        return () => {
          unsubscribe();
          release();
        };
      },
    };
  }

  return {
    status: shared(status),
    resync: shared(resync),
    on<T extends EventType>(
      type: T,
      listener: (event: EventOf<T>) => void,
    ): () => void {
      const dispatch = (event: EventEnvelope) => {
        if (event.type === type) listener(event as EventOf<T>);
      };
      listeners.add(dispatch);
      const release = retain();
      return () => {
        listeners.delete(dispatch);
        release();
      };
    },
  };
}

/**
 * Register `on(type, handler)` and subscribe to `status`/`resync` as needed;
 * all three return cleanup functions and share one connection. Fetch initial
 * snapshots once open. On a new resync epoch the replacement stream is already
 * open: refetch snapshots and buffer live events until they arrive, then apply
 * them idempotently without replacing newer snapshot state.
 */
export const events = createEventStore();
