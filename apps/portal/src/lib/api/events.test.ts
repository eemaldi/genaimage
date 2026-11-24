import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createEventStore } from './events';
import type { ResyncEvent } from './events';

class FakeEventSource extends EventTarget {
  static connections: FakeEventSource[] = [];
  close = vi.fn();

  constructor(readonly url: string) {
    super();
    FakeEventSource.connections.push(this);
  }

  emit(type: string, data: unknown, lastEventId = '') {
    this.dispatchEvent(
      new MessageEvent(type, { data: JSON.stringify(data), lastEventId }),
    );
  }

  opened() {
    this.dispatchEvent(new Event('open'));
  }
  fail() {
    this.dispatchEvent(new Event('error'));
  }
}

const streams = () => FakeEventSource.connections;
const health = (bus_id = 'bus-a') => ({
  type: 'backend_health_changed',
  bus_id,
  id: 1,
  timestamp: '2026-01-01T00:00:00Z',
  backend: 'comfyui',
  health: null,
  error: 'Unavailable',
});

describe('portal event stream', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeEventSource.connections = [];
  });
  afterEach(() => vi.useRealTimers());

  it('opens lazily, dispatches typed named events and shares one connection', () => {
    const store = createEventStore(FakeEventSource);
    expect(streams()).toHaveLength(0);
    const status = vi.fn();
    const stopStatus = store.status.subscribe(status);
    const receive = vi.fn();
    const stopEvent = store.on('backend_health_changed', (event) =>
      receive(event.backend),
    );
    const stopResync = store.resync.subscribe(vi.fn());
    expect(streams()).toHaveLength(1);
    const [stream] = streams();
    expect(status).toHaveBeenLastCalledWith('connecting');
    stream.opened();
    expect(status).toHaveBeenLastCalledWith('open');
    stream.emit(
      'settings_changed',
      { type: 'settings_changed', bus_id: 'bus-a', id: 1, settings: {} },
      '1',
    );
    expect(receive).not.toHaveBeenCalled();
    stream.emit('backend_health_changed', health(), '2');
    expect(receive).toHaveBeenCalledExactlyOnceWith('comfyui');
    stopStatus();
    stopResync();
    expect(stream.close).not.toHaveBeenCalled();
    stopEvent();
    stopEvent();
    expect(stream.close).toHaveBeenCalledTimes(1);
    stream.emit('backend_health_changed', health(), '3');
    expect(receive).toHaveBeenCalledTimes(1);
    const stop = store.status.subscribe(status);
    expect(streams()[1].url).toBe('/api/v1/events');
    stop();
  });

  it('resumes using the exact wire ID and backs off until a connection opens', () => {
    const store = createEventStore(FakeEventSource);
    const status = vi.fn();
    const stop = store.status.subscribe(status);
    const [first] = streams();
    first.opened();
    first.emit('backend_health_changed', health(), '18446744073709551615');
    first.fail();
    first.fail(); // An error from a closed source must not schedule another connection.
    expect(first.close).toHaveBeenCalledTimes(1);
    expect(status).toHaveBeenLastCalledWith('reconnecting');
    vi.advanceTimersByTime(249);
    expect(streams()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    const second = streams()[1];
    expect(second.url).toBe(
      '/api/v1/events?last_event_id=18446744073709551615',
    );
    second.fail();
    vi.advanceTimersByTime(499);
    expect(streams()).toHaveLength(2);
    vi.advanceTimersByTime(1);
    const third = streams()[2];
    expect(third.url).toBe(second.url);
    third.opened();
    third.fail();
    vi.advanceTimersByTime(250);
    expect(streams()).toHaveLength(4);
    stop();
  });

  const reasons = [
    { reason: 'lagged', skipped: 5 },
    { reason: 'cursor_out_of_range', requested: 2, oldest: 4, latest: 5 },
    { reason: 'invalidated', id: 42 },
  ] satisfies ResyncEvent[];

  it.each(reasons)(
    'opens a fresh stream and signals a snapshot refetch for $reason',
    (reason) => {
      const store = createEventStore(FakeEventSource);
      const resync = vi.fn();
      const stop = store.resync.subscribe(resync);
      const [first] = streams();
      first.opened();
      first.emit('backend_health_changed', health(), '1');
      first.emit('resync', reason);
      expect(first.close).toHaveBeenCalledOnce();
      expect(resync).toHaveBeenLastCalledWith(null);
      vi.advanceTimersByTime(250);
      const second = streams()[1];
      expect(second.url).toBe('/api/v1/events');
      second.opened();
      expect(resync).toHaveBeenLastCalledWith({ epoch: 1, reason });
      second.emit('resync', reason);
      vi.advanceTimersByTime(250);
      streams()[2].opened();
      expect(resync).toHaveBeenLastCalledWith({ epoch: 2, reason });
      stop();
    },
  );

  it('resynchronises on a changed bus instead of dispatching the replayed event', () => {
    const store = createEventStore(FakeEventSource);
    const resync = vi.fn();
    const receive = vi.fn();
    const stopResync = store.resync.subscribe(resync);
    const stopEvent = store.on('backend_health_changed', receive);
    const [first] = streams();
    first.opened();
    first.emit('backend_health_changed', health(), '1');
    first.fail();
    vi.advanceTimersByTime(250);
    const second = streams()[1];
    second.opened();
    second.emit('backend_health_changed', health('bus-b'), '2');
    expect(receive).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(250);
    expect(streams()[2].url).toBe('/api/v1/events');
    streams()[2].opened();
    expect(resync).toHaveBeenLastCalledWith({
      epoch: 1,
      reason: { reason: 'bus_changed', requested: 'bus-a', current: 'bus-b' },
    });
    stopResync();
    stopEvent();
  });

  it('resynchronises after a disconnect without a cursor and cancels idle reconnects', () => {
    const store = createEventStore(FakeEventSource);
    const resync = vi.fn();
    const stop = store.resync.subscribe(resync);
    streams()[0].opened();
    streams()[0].fail();
    vi.advanceTimersByTime(250);
    expect(streams()[1].url).toBe('/api/v1/events');
    streams()[1].opened();
    expect(resync).toHaveBeenLastCalledWith({
      epoch: 1,
      reason: { reason: 'cursor_unavailable' },
    });
    streams()[1].fail();
    stop();
    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(10_000);
    expect(streams()).toHaveLength(2);
  });
});
