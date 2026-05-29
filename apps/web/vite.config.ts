import { sveltekit } from '@sveltejs/kit/vite';
/// <reference types="vitest" />
import { defineConfig } from 'vitest/config';

// Vite proxy for dev: /api/* → gt-api on :8787 (docker compose service).
// Override with VITE_GT_API_URL env if running gt-api elsewhere.
const gtApiUrl = process.env.VITE_GT_API_URL ?? 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: gtApiUrl,
        changeOrigin: true,
        ws: true
      },
      '/metrics': { target: gtApiUrl, changeOrigin: true },
      '/health': { target: gtApiUrl, changeOrigin: true },
      '/readyz': { target: gtApiUrl, changeOrigin: true }
    }
  },
  test: {
    include: ['src/**/*.{test,spec}.{js,ts}'],
    environment: 'jsdom'
  }
});
