import type { Component } from 'svelte';
import Dashboard from './routes/dashboard/Page.svelte';
import Generate from './routes/generate/Page.svelte';
import Jobs from './routes/jobs/Page.svelte';
import Gallery from './routes/gallery/Page.svelte';
import Workflows from './routes/workflows/Page.svelte';
import WorkflowImport from './routes/workflows/import/Page.svelte';
import WorkflowDetail from './routes/workflows/Detail.svelte';
import Assistant from './routes/assistant/Page.svelte';
import Diagnostics from './routes/diagnostics/Page.svelte';
import Models from './routes/models/Page.svelte';
import Settings from './routes/settings/Page.svelte';
import Setup from './routes/setup/Page.svelte';
import BatchDetail from './routes/batches/Detail.svelte';
import RunDetail from './routes/runs/Detail.svelte';
import NotFound from './routes/not-found/Page.svelte';

export type RouteProps = {
  params: Record<string, string>;
  query: URLSearchParams;
};
type Route = { pattern: string; component: Component<RouteProps> };

const routes: Route[] = [
  { pattern: '/', component: Dashboard },
  { pattern: '/generate', component: Generate },
  { pattern: '/jobs', component: Jobs },
  { pattern: '/gallery', component: Gallery },
  { pattern: '/workflows', component: Workflows },
  { pattern: '/workflows/import', component: WorkflowImport },
  { pattern: '/workflows/:id', component: WorkflowDetail },
  { pattern: '/assistant', component: Assistant },
  { pattern: '/diagnostics', component: Diagnostics },
  { pattern: '/models', component: Models },
  { pattern: '/settings', component: Settings },
  { pattern: '/setup', component: Setup },
  { pattern: '/runs/:id', component: RunDetail },
  { pattern: '/batches/:id', component: BatchDetail },
];

export function matchHash(hash: string): RouteProps & {
  component: Component<RouteProps>;
  path: string;
  pattern?: string;
} {
  const [path = '/', ...queryParts] = (hash.replace(/^#/, '') || '/').split(
    '?',
  );
  const query = new URLSearchParams(queryParts.join('?'));
  for (const route of routes) {
    const segments = path.split('/');
    const pattern = route.pattern.split('/');
    if (segments.length !== pattern.length) continue;
    const params: Record<string, string> = {};
    let matches = true;
    for (let i = 0; i < pattern.length; i++) {
      const part = pattern[i];
      const segment = segments[i];
      if (part.startsWith(':') && segment) {
        try {
          params[part.slice(1)] = decodeURIComponent(segment);
        } catch {
          matches = false;
          break;
        }
      } else if (part !== segment) {
        matches = false;
        break;
      }
    }
    if (matches)
      return {
        component: route.component,
        params,
        query,
        path,
        pattern: route.pattern,
      };
  }
  return { component: NotFound, params: {}, query, path };
}
