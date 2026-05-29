import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  resolve: {
    alias: {
      $lib: path.resolve('./src/lib'),
    },
  },
  server: {
    port: 5173,
    strictPort: false,
    // Proxy roy-management HTTP API (default :8079) so the browser can call
    // /management/agents without CORS in dev. Override with VITE_ROY_MGMT_URL.
    proxy: {
      '/management': {
        target: process.env.VITE_ROY_MGMT_URL ?? 'http://127.0.0.1:8079',
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/management/, ''),
      },
    },
  },
});
