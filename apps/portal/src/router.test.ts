import { describe, expect, it } from 'vitest';
import { matchHash } from './router';
import Dashboard from './routes/dashboard/Page.svelte';
import Generate from './routes/generate/Page.svelte';
import Jobs from './routes/jobs/Page.svelte';
import Gallery from './routes/gallery/Page.svelte';
import Assistant from './routes/assistant/Page.svelte';
import Settings from './routes/settings/Page.svelte';
import Workflows from './routes/workflows/Page.svelte';
import WorkflowDetail from './routes/workflows/Detail.svelte';
import RunDetail from './routes/runs/Detail.svelte';
import NotFound from './routes/not-found/Page.svelte';

describe('hash routing', () => {
  it('matches the default and exact paths with query state', () => {
    expect(matchHash('').component).toBe(Dashboard);
    expect(matchHash('#/workflows').component).toBe(Workflows);
    const route = matchHash('#/generate?workflow=example%20one&tag=a&tag=b');
    expect(route.component).toBe(Generate);
    expect(route.query.get('workflow')).toBe('example one');
    expect(route.query.getAll('tag')).toEqual(['a', 'b']);
  });

  it.each([
    ['#/', Dashboard],
    ['#/generate', Generate],
    ['#/jobs', Jobs],
    ['#/gallery', Gallery],
    ['#/workflows', Workflows],
    ['#/assistant', Assistant],
    ['#/settings', Settings],
  ])('registers %s', (hash, component) => {
    expect(matchHash(hash).component).toBe(component);
  });

  it('decodes detail parameters independently of query strings', () => {
    const workflow = matchHash('#/workflows/my%20workflow?tab=info');
    expect(workflow.component).toBe(WorkflowDetail);
    expect(workflow.params).toEqual({ id: 'my workflow' });
    expect(workflow.query.get('tab')).toBe('info');
    const run = matchHash('#/runs/a%2Fb');
    expect(run.component).toBe(RunDetail);
    expect(run.params.id).toBe('a/b');
  });

  it.each(['#/unknown', '#/runs/', '#/workflows/a/b', '#/runs/%xx'])(
    'returns not found for %s',
    (hash) => {
      expect(matchHash(hash).component).toBe(NotFound);
    },
  );
});
