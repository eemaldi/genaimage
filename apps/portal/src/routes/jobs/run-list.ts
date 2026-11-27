import { writable } from 'svelte/store';
import type { api, RunQuery } from '../../lib/api/client';
import type { EventEnvelope, events } from '../../lib/api/events';
import type { Run } from './run-model';

const eventTypes = [
  'run_queued',
  'run_started',
  'run_progress',
  'run_completed',
  'run_failed',
  'run_cancelled',
  'run_unknown_submission',
] as const;
type RunEvent = Extract<EventEnvelope, { type: (typeof eventTypes)[number] }>;
export type RunRow = {
  run: Run;
  node?: string;
  progress?: { value: number; max: number };
};
type List = {
  rows: RunRow[];
  loading: boolean;
  error?: unknown;
  next?: string | null;
};

export const pageSize = 25;

// Date.parse truncates the fractional precision used by the durable timestamps.
export function compareTime(a: string, b: string) {
  const subMilliseconds = (time: string) =>
    (time.match(/\.\d{3}(\d+)/)?.[1] ?? '').padEnd(6, '0');
  return (
    Date.parse(a) - Date.parse(b) ||
    subMilliseconds(a).localeCompare(subMilliseconds(b))
  );
}

export function mergeRunEvent(row: RunRow, event: RunEvent): RunRow {
  const order = compareTime(event.run.updated_at, row.run.updated_at);
  if (order < 0 || (order === 0 && event.run.status !== row.run.status))
    return row;
  const run = order > 0 ? event.run : row.run;
  if (run.status !== 'running') return { run };
  const next = run.status === row.run.status ? { ...row, run } : { run };
  const backend =
    event.type === 'run_progress' ? event.backend_event : undefined;
  if (!backend) return next;
  const { node_id, value, maximum, event_type } = backend;
  if (event_type === 'executing') {
    return { run, node: node_id ?? undefined };
  }
  if (event_type === 'progress') {
    return {
      run,
      node: node_id ?? next.node,
      progress:
        typeof value === 'number' &&
        typeof maximum === 'number' &&
        maximum > 0 &&
        value >= 0 &&
        value <= maximum
          ? { value, max: maximum }
          : undefined,
    };
  }
  return next;
}

export function createRunList(
  client: Pick<typeof api, 'listRuns'>,
  source: Pick<typeof events, 'on' | 'status' | 'resync'>,
) {
  let state: List = { rows: [], loading: true };
  const { subscribe, set } = writable(state);
  let query: RunQuery = {};
  let request: AbortController | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let pending: RunEvent[] = [];
  let overflow = false;
  let ready = false;
  let stopped = false;
  const membershipChecks = new Map<string, string>();

  function publish(next: List) {
    set((state = next));
  }

  function matches(run: Run) {
    return (
      (!query?.status || query.status === run.status) &&
      (!query?.workflow || query.workflow === run.workflow_id) &&
      (!query?.batch || query.batch === run.batch_id)
    );
  }

  function scheduleRefresh() {
    timer ??= setTimeout(() => {
      void refresh();
    }, 100);
  }

  function apply(event: RunEvent) {
    const index = state.rows.findIndex(({ run }) => run.id === event.run.id);
    if (index < 0) {
      // Let the server decide membership and ordering for new/filtered rows.
      if (
        matches(event.run) &&
        (!state.next ||
          compareTime(
            event.run.created_at,
            state.rows.at(-1)?.run.created_at ?? '',
          ) >= 0)
      ) {
        const { id, updated_at, status } = event.run;
        const revision = `${updated_at}/${status}`;
        if (membershipChecks.get(id) === revision) return;
        membershipChecks.delete(id);
        membershipChecks.set(id, revision);
        if (membershipChecks.size > 512) {
          const oldest = membershipChecks.keys().next();
          if (!oldest.done) membershipChecks.delete(oldest.value);
        }
        scheduleRefresh();
      }
      return;
    }
    const row = mergeRunEvent(state.rows[index], event);
    if (row === state.rows[index]) return;
    if (!matches(row.run)) {
      publish({
        ...state,
        rows: state.rows.filter(({ run }) => run.id !== row.run.id),
      });
      scheduleRefresh();
    } else {
      publish({
        ...state,
        rows: state.rows.map((old, i) => (i === index ? row : old)),
      });
    }
  }

  async function refresh(nextQuery?: RunQuery) {
    if (stopped) return;
    clearTimeout(timer);
    timer = undefined;
    request?.abort();
    const controller = new AbortController();
    request = controller;
    if (nextQuery) {
      query = nextQuery;
      membershipChecks.clear();
      pending = [];
      overflow = false;
    }
    publish({
      ...(nextQuery ? { rows: [] } : state),
      loading: true,
      error: undefined,
    });
    try {
      const page = await client.listRuns(
        { ...query, limit: pageSize },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      const previous = new Map(state.rows.map((row) => [row.run.id, row]));
      const rows = page.runs.map((run) => {
        const old = previous.get(run.id);
        if (!old) return { run };
        const order = compareTime(old.run.updated_at, run.updated_at);
        if (order > 0 || (order === 0 && run.status === old.run.status))
          return old;
        return { run };
      });
      ready = true;
      publish({ rows, loading: false, next: page.next_cursor });
    } catch (error) {
      if (controller.signal.aborted) return;
      publish({ ...state, loading: false, error });
    } finally {
      if (!controller.signal.aborted) {
        request = undefined;
        const buffered = pending;
        pending = [];
        if (ready) for (const event of buffered) apply(event);
        if (overflow) scheduleRefresh();
        overflow = false;
      }
    }
  }

  function receive(event: RunEvent) {
    if (request || !ready) {
      if (pending.length < 512) pending.push(event);
      else overflow = true;
    } else apply(event);
  }

  const cleanup = eventTypes.map((type) => source.on(type, receive));
  let opened = false;
  cleanup.push(
    source.status.subscribe((status) => {
      if (status !== 'open' || opened) return;
      opened = true;
      if (request || ready || state.error) void refresh();
    }),
  );
  let epoch = 0;
  cleanup.push(
    source.resync.subscribe((signal) => {
      if (!signal || signal.epoch === epoch) return;
      epoch = signal.epoch;
      membershipChecks.clear();
      pending = [];
      overflow = false;
      publish({ ...state, rows: state.rows.map(({ run }) => ({ run })) });
      void refresh();
    }),
  );

  return {
    subscribe,
    refresh,
    update(run: Run) {
      const old = state.rows.find((row) => row.run.id === run.id);
      if (old && compareTime(run.updated_at, old.run.updated_at) >= 0) {
        const rows = matches(run)
          ? state.rows.map((row) =>
              row === old
                ? run.status === old.run.status
                  ? { ...row, run }
                  : { run }
                : row,
            )
          : state.rows.filter((row) => row !== old);
        publish({ ...state, rows });
      }
      void refresh();
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
      request?.abort();
      cleanup.forEach((dispose) => dispose());
    },
  };
}
