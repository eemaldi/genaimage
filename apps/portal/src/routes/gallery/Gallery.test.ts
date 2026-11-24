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
import { writable } from 'svelte/store';
import App from '../../App.svelte';
import { api, createApiClient } from '../../lib/api/client';
import {
  events,
  type EventEnvelope,
  type ResyncSignal,
} from '../../lib/api/events';
import type { components } from '../../lib/api/schema';
import { mockNativeDialog } from '../../test/dialog';
import type { Output } from './gallery-model';
import Grid from './Grid.svelte';
import { gridGeometry, virtualRows } from './virtualiser';
import { formatRecordedValue, parameterDiff } from './parameter-diff';

vi.mock('../../lib/api/client', async (original) => {
  const actual = await original<typeof import('../../lib/api/client')>();
  return {
    ...actual,
    api: {
      getRecordedRun: vi.fn(),
      listOutputs: vi.fn(),
      getOutputMetadata: vi.fn(),
    },
  };
});

vi.mock('../../lib/api/events', () => ({
  events: {
    on: vi.fn(),
    resync: { subscribe: vi.fn() },
    status: { subscribe: vi.fn() },
  },
}));

mockNativeDialog();
const resync = writable<ResyncSignal | null>(null);
const listeners = new Map<string, (event: EventEnvelope) => void>();
const list = vi.mocked(api.listOutputs);
const metadata = vi.mocked(api.getOutputMetadata);

function output(i: number, contentType = 'image/png'): Output {
  return {
    id: `run-${i}:${i}`,
    run_id: `run-${i}`,
    workflow_id: 'landscape',
    node_id: '1',
    kind: 'image',
    source_filename: `File ${i}`,
    content_type: contentType,
    byte_size: 20,
    sha256: 'example',
    created_at: new Date(Date.UTC(2026, 0, 1, 0, 0, i)).toISOString(),
    tags: ['Travel'],
  };
}

function run(
  id: string,
  parameters: unknown,
): components['schemas']['RunResponse'] {
  return {
    id,
    parameters,
    prompt_id: 'prompt-id',
    status: 'completed',
    created_at: '',
    updated_at: '',
    workflow_id: 'landscape',
    workflow_version: '1',
    workflow_sha256: 'checksum',
  };
}

function page(outputs: Output[], next_cursor: string | null = null) {
  return { outputs, next_cursor };
}

function route(hash = '#/gallery') {
  history.replaceState(null, '', hash);
  render(App);
}

beforeEach(() => {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
    },
  );
  vi.resetAllMocks();
  resync.set(null);
  listeners.clear();
  vi.mocked(events.resync.subscribe).mockImplementation(resync.subscribe);
  vi.mocked(events.status.subscribe).mockImplementation((listener) => {
    listener('open');
    return () => {};
  });
  vi.mocked(events.on).mockImplementation((type, listener) => {
    listeners.set(type, listener as (event: EventEnvelope) => void);
    return () => {
      listeners.delete(type);
    };
  });
  list.mockResolvedValue(page([output(2), output(1)]));
  metadata.mockImplementation(async (id) =>
    output(Number(id.split(':').at(-1))),
  );
  vi.mocked(api.getRecordedRun).mockImplementation(async (id) =>
    run(id, { seed: id === 'run-2' ? 20 : 10, steps: 4 }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function dimensions(width: number, height: number) {
  const widthSpy = vi
    .spyOn(HTMLElement.prototype, 'clientWidth', 'get')
    .mockReturnValue(width);
  const heightSpy = vi
    .spyOn(HTMLElement.prototype, 'clientHeight', 'get')
    .mockReturnValue(height);
  return () => {
    widthSpy.mockRestore();
    heightSpy.mockRestore();
  };
}

async function scroll(viewport: HTMLElement, top: number) {
  viewport.scrollTop = top;
  await fireEvent.scroll(viewport);
}

describe('gallery virtualisation', () => {
  it('clamps boundaries, computes complete overscanned rows and handles a partial final row', () => {
    expect(gridGeometry(320).columns).toBe(1);
    expect(gridGeometry(720).columns).toBe(4);
    expect(virtualRows(0, 4, 100, 999, 200)).toEqual({
      start: 0,
      end: 0,
      offset: 0,
      totalHeight: 0,
    });
    expect(virtualRows(80, 4, 100, 450, 200)).toEqual({
      start: 8,
      end: 36,
      offset: 200,
      totalHeight: 2000,
    });
    expect(virtualRows(19, 4, 100, 900, 200)).toEqual({
      start: 4,
      end: 19,
      offset: 100,
      totalHeight: 500,
    });
  });

  it('keeps the DOM bounded while scrolling across 5,000 outputs', async () => {
    const restore = dimensions(900, 540);
    try {
      const outputs = Array.from({ length: 5000 }, (_, i) => output(i + 1));
      render(Grid, {
        outputs,
        selected: new Map(),
        href: (id) => `#${id}`,
        toggle: vi.fn(),
        more: vi.fn(),
        canLoad: false,
      });
      const viewport = screen.getByRole('region', { name: 'Gallery outputs' });
      const { rowHeight } = gridGeometry(900);
      let max = 0;
      for (const row of [0, 1, 20, 100, 340, 700, 996, 997, 0]) {
        await scroll(viewport, row * rowHeight);
        expect(
          within(viewport).getByRole('link', {
            name: `Open File ${row * 5 + 1}`,
          }),
        ).toBeTruthy();
        max = Math.max(max, within(viewport).getAllByRole('listitem').length);
      }
      expect(max).toBeLessThanOrEqual(35);
    } finally {
      restore();
    }
  });
});

it('reads and writes gallery filters through the router hash', async () => {
  route('#/gallery?workflow=old&tag=Travel&from=2026-01-01&status=failed');
  await screen.findByRole('heading', { name: 'Gallery' });
  const workflow = await screen.findByLabelText('Workflow ID');
  expect((workflow as HTMLInputElement).value).toBe('old');
  expect(list).toHaveBeenCalledWith(
    expect.objectContaining({
      workflow: 'old',
      tag: 'Travel',
      created_from: '2026-01-01T00:00:00Z',
      status: 'failed',
    }),
    expect.anything(),
  );
  await fireEvent.input(workflow, { target: { value: 'new workflow' } });
  await fireEvent.input(screen.getByLabelText('To date (UTC)'), {
    target: { value: '2026-01-31' },
  });
  await fireEvent.change(screen.getByLabelText('Run status'), {
    target: { value: 'cancelled' },
  });
  await fireEvent.click(screen.getByRole('button', { name: 'Apply filters' }));
  expect(location.hash).toBe(
    '#/gallery?workflow=new+workflow&from=2026-01-01&to=2026-01-31&tag=Travel&status=cancelled',
  );
  await waitFor(() =>
    expect(list).toHaveBeenCalledWith(
      expect.objectContaining({
        workflow: 'new workflow',
        created_to: '2026-01-31T23:59:59.999999999Z',
        status: 'cancelled',
      }),
      expect.anything(),
    ),
  );
});

it('loads the next cursor when scrolled near the end', async () => {
  list.mockImplementation(async (query) =>
    query?.cursor === 'opaque'
      ? page([output(1)])
      : page(
          Array.from({ length: 100 }, (_, i) => output(101 - i)),
          'opaque',
        ),
  );
  route();
  const viewport = await screen.findByRole('region', {
    name: 'Gallery outputs',
  });
  expect(list).toHaveBeenCalledTimes(1);
  await scroll(viewport, gridGeometry(720).rowHeight * 23);
  await within(viewport).findByRole('link', { name: 'Open File 1' });
  expect(list).toHaveBeenLastCalledWith(
    expect.objectContaining({ cursor: 'opaque' }),
    expect.anything(),
  );
});

it('opens a deep link outside the loaded pages and exposes native media and the download', async () => {
  metadata.mockResolvedValue(output(87, 'video/mp4'));
  route('#/gallery?output=run-87%3A87');
  const dialog = await screen.findByRole('dialog', { name: 'Output preview' });
  expect(
    await within(dialog).findByRole('heading', { name: 'File 87' }),
  ).toBeTruthy();
  expect(metadata).toHaveBeenCalledWith('run-87:87', expect.anything());
  expect(dialog.querySelector('video')?.controls).toBe(true);
  expect(
    within(dialog)
      .getByRole('link', { name: 'Download original' })
      .getAttribute('download'),
  ).toBe('File 87');
  expect(
    within(dialog).getByRole('link', { name: 'View run' }).getAttribute('href'),
  ).toBe('#/runs/run-87');
  await fireEvent(dialog, new Event('cancel', { cancelable: true }));
  await waitFor(() => expect(location.hash).toBe('#/gallery'));
  await waitFor(() => expect(dialog.hasAttribute('open')).toBe(false));
});

function twoPages() {
  const first = Array.from({ length: 100 }, (_, i) => output(102 - i));
  list.mockImplementation(async (query) =>
    query?.cursor === 'second'
      ? page([output(2), output(1)])
      : page(first, 'second'),
  );
}

it('keeps Next focused across a delayed cursor page and accepts ArrowRight afterwards', async () => {
  const first = Array.from({ length: 100 }, (_, i) => output(102 - i));
  let resolvePage!: (value: ReturnType<typeof page>) => void;
  const delayed = new Promise<ReturnType<typeof page>>((resolve) => {
    resolvePage = resolve;
  });
  list.mockImplementation(async (query) =>
    query?.cursor === 'second' ? delayed : page(first, 'second'),
  );
  route('#/gallery?output=run-3%3A3');
  const dialog = await screen.findByRole('dialog', { name: 'Output preview' });
  const next = within(dialog).getByRole('button', { name: 'Next' });
  await waitFor(() => expect(next.getAttribute('aria-disabled')).toBe('false'));
  next.focus();
  const user = userEvent.setup();
  await user.keyboard('{Enter}');
  expect(within(dialog).getByText('Loading the next output…')).toBeTruthy();
  expect(next.getAttribute('aria-disabled')).toBe('true');
  expect((next as HTMLButtonElement).disabled).toBe(false);
  expect(document.activeElement).toBe(next);
  await user.keyboard('{Enter}{ArrowRight}');
  expect(
    list.mock.calls.filter(([query]) => query?.cursor === 'second'),
  ).toHaveLength(1);
  expect(location.hash).toBe('#/gallery?output=run-3%3A3');

  resolvePage(page([output(2), output(1)]));
  await within(dialog).findByRole('heading', { name: 'File 2' });
  expect(document.activeElement).toBe(next);
  await user.keyboard('{ArrowRight}');
  await within(dialog).findByRole('heading', { name: 'File 1' });
  expect(document.activeElement).toBe(next);
  expect(next.getAttribute('aria-disabled')).toBe('true');
  expect(location.hash).toBe('#/gallery?output=run-1%3A1');
});

it('does not navigate to an unrelated output before a deep-linked position is known', async () => {
  twoPages();
  metadata.mockResolvedValue(output(2));
  route('#/gallery?output=run-2%3A2');
  const dialog = await screen.findByRole('dialog', { name: 'Output preview' });
  await within(dialog).findByRole('heading', { name: 'File 2' });
  expect(
    within(dialog).getByText(/unavailable until this output appears/),
  ).toBeTruthy();
  expect(
    (
      within(dialog).getByRole('button', {
        name: 'Previous',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  const next = within(dialog).getByRole('button', { name: 'Next' });
  expect(next.getAttribute('aria-disabled')).toBe('true');
  await fireEvent.keyDown(
    within(dialog).getByRole('button', { name: 'Close' }),
    { key: 'ArrowRight' },
  );
  expect(location.hash).toBe('#/gallery?output=run-2%3A2');
  const grid = await screen.findByRole('region', { name: 'Gallery outputs' });
  await scroll(grid, gridGeometry(720).rowHeight * 23);
  await waitFor(() => expect(next.getAttribute('aria-disabled')).toBe('false'));
  await fireEvent.click(next);
  expect(
    await within(dialog).findByRole('heading', { name: 'File 1' }),
  ).toBeTruthy();
});

it('navigates with arrow keys and pans rather than navigating when zoomed', async () => {
  route();
  await userEvent.click(
    await screen.findByRole('link', { name: 'Open File 2' }),
  );
  const dialog = await screen.findByRole('dialog', { name: 'Output preview' });
  const close = within(dialog).getByRole('button', { name: 'Close' });
  await fireEvent.keyDown(close, { key: '+' });
  expect(within(dialog).getByLabelText('Zoom level').textContent).toContain(
    '125%',
  );
  await fireEvent.keyDown(close, { key: 'ArrowRight' });
  expect(within(dialog).getByRole('heading', { name: 'File 2' })).toBeTruthy();
  await fireEvent.keyDown(close, { key: '0' });
  await fireEvent.keyDown(close, { key: 'ArrowRight' });
  expect(
    await within(dialog).findByRole('heading', { name: 'File 1' }),
  ).toBeTruthy();
  await fireEvent.keyDown(close, { key: 'ArrowLeft' });
  expect(
    await within(dialog).findByRole('heading', { name: 'File 2' }),
  ).toBeTruthy();
});

it('compares keyboard-selected outputs and collapses identical recorded fields', async () => {
  route();
  const first = await screen.findByRole('checkbox', {
    name: 'Select File 2 for comparison',
  });
  first.focus();
  const user = userEvent.setup();
  await user.keyboard(' ');
  screen
    .getByRole('checkbox', { name: 'Select File 1 for comparison' })
    .focus();
  await user.keyboard(' ');
  await user.click(
    screen.getByRole('button', { name: 'Compare selected (2)' }),
  );
  const dialog = await screen.findByRole('dialog', { name: 'Compare outputs' });
  const table = await within(dialog).findByRole('table', {
    name: 'Parameter differences',
  });
  expect(within(table).getByRole('rowheader', { name: 'Seed' })).toBeTruthy();
  expect(
    within(table)
      .getAllByRole('cell')
      .map((cell) => cell.textContent),
  ).toEqual(['20', '10']);
  expect(within(table).queryByRole('rowheader', { name: 'steps' })).toBeNull();
  const identical = within(dialog).getByText(/Identical recorded fields/, {
    selector: 'summary',
  });
  expect(identical.parentElement?.hasAttribute('open')).toBe(false);
  await user.click(identical);
  expect(identical.parentElement?.hasAttribute('open')).toBe(true);
  expect(within(dialog).getByRole('rowheader', { name: 'steps' })).toBeTruthy();
});

it('distinguishes absent values from recorded null and compares JSON without relying on key order', () => {
  const rows = parameterDiff([
    run('a', { seed: null, config: { a: 0, b: false } }),
    run('b', { config: { b: false, a: 0 } }),
  ]);
  expect(rows.find((row) => row.label === 'Seed')).toMatchObject({
    values: [null, undefined],
    different: true,
  });
  expect(rows.find((row) => row.label === 'config')?.different).toBe(false);
});

it('compares large seeds without losing digits from the run response', async () => {
  const client = createApiClient(async (url) => {
    const id = String(url).split('/').at(-1)!;
    const seed =
      id === 'distinct' ? '18446744073709551614' : '18446744073709551615';
    const json = JSON.stringify(run(id, null)).replace(
      '"parameters":null',
      `"parameters":{"seed":${seed}}`,
    );
    return new Response(json);
  });
  const [first, identical, distinct] = await Promise.all(
    ['first', 'identical', 'distinct'].map((id) => client.getRecordedRun(id)),
  );
  const seed = (other: typeof first) =>
    parameterDiff([first, other]).find((row) => row.label === 'Seed')!;
  expect(seed(identical)).toMatchObject({ different: false, inexact: false });
  expect(seed(distinct)).toMatchObject({ different: true, inexact: false });
  expect(seed(distinct).values.map(formatRecordedValue)).toEqual([
    '18446744073709551615',
    '18446744073709551614',
  ]);
  expect(
    parameterDiff([
      run('a', { seed: Number.MAX_SAFE_INTEGER + 1 }),
      run('b', { seed: 2 }),
    ]).find((row) => row.label === 'Seed'),
  ).toMatchObject({ different: false, inexact: true });
});

it('uses fallbacks for unsupported thumbnail formats and failed requests', async () => {
  list.mockResolvedValue(
    page([output(3, 'audio/flac'), output(2, 'image/gif'), output(1)]),
  );
  route();
  const unsupported = await screen.findByRole('link', { name: 'Open File 2' });
  expect(within(unsupported).getByText('Image · no thumbnail')).toBeTruthy();
  expect(
    within(screen.getByRole('link', { name: 'Open File 3' })).getByText(
      'Audio',
    ),
  ).toBeTruthy();
  const retry = screen.getByRole('link', { name: 'Open File 1' });
  const image = retry.querySelector('img')!;
  expect(image.getAttribute('loading')).toBe('lazy');
  await fireEvent.error(image);
  expect(within(retry).getByText('Image · no thumbnail')).toBeTruthy();
});

it('merges a live run without duplicates, preserves the scrolled row and refetches on resync', async () => {
  const initial = Array.from({ length: 60 }, (_, i) => output(60 - i));
  const arrivals = Array.from({ length: 4 }, (_, i) => output(64 - i));
  const { rowHeight } = gridGeometry(720);
  list.mockResolvedValue(page(initial));
  route();
  const viewport = await screen.findByRole('region', {
    name: 'Gallery outputs',
  });
  await scroll(viewport, rowHeight * 4 + 20);
  list.mockImplementation(async (query) =>
    query?.run
      ? page([...arrivals, initial[0]])
      : page([...arrivals, ...initial]),
  );
  const record = run('run-64', {});
  listeners.get('run_completed')?.({
    type: 'run_completed',
    run: record,
  } as EventEnvelope);
  await waitFor(() => expect(viewport.scrollTop).toBe(rowHeight * 5 + 20));
  await scroll(viewport, 0);
  expect(screen.getAllByRole('link', { name: 'Open File 60' })).toHaveLength(1);
  expect(screen.getByRole('link', { name: 'Open File 64' })).toBeTruthy();
  list.mockResolvedValue(page([output(65), ...arrivals, ...initial]));
  resync.set({ epoch: 1, reason: { reason: 'cursor_unavailable' } });
  expect(
    await screen.findByRole('link', { name: 'Open File 65' }),
  ).toBeTruthy();
});
