import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `pnpm dev` serves the UI with hot reload and sends /api to a hopperd
// running locally (HOPPERD_URL to point elsewhere).
const target = process.env.HOPPERD_URL || 'http://127.0.0.1:8790';

export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: { '/api': { target, ws: true, changeOrigin: false } },
  },
  build: { outDir: 'dist', emptyOutDir: true, chunkSizeWarningLimit: 300 },
});
