import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  resolve: { conditions: ['browser'] },
  server: {
    host: '127.0.0.1',
    proxy: {
      '^/api(?:/|\\?|$)': {
        target: 'http://127.0.0.1:8190',
        changeOrigin: true,
        configure(proxy) {
          proxy.on('proxyReq', (proxyRequest, request) => {
            // Preserve cross-origin requests for the API's own origin check.
            if (request.headers.origin === `http://${request.headers.host}`) {
              proxyRequest.setHeader('Origin', 'http://127.0.0.1:8190');
            }
          });
        },
      },
    },
  },
  test: { environment: 'jsdom' },
});
