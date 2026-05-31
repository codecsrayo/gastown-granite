// hq-fe-build.7 — Playwright e2e bootstrap. `pnpm test:e2e` boots the SvelteKit
// preview server against the production build (so the dev HMR overlay and source
// maps do not interfere with assertions) and runs every `*.spec.ts` under `e2e/`.
// `VITE_GT_API_URL` propagates through the build so the harness can point at a
// fixture gateway when one is available; absent, the proxy targets the default
// `127.0.0.1:8787` and the suite skips API-bound checks.
//
// One Chromium project today: WebKit / Firefox can be added once the suite grows.
// CI can flip `reporter` to `'list'` and tighten `retries`; defaults match the
// `pnpm test:e2e` developer-local experience.

import { defineConfig, devices } from '@playwright/test';

const PORT = Number(process.env.PORT ?? 4173);
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? 'list' : 'html',
  timeout: 30_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure'
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] }
    }
  ],
  webServer: {
    command: `pnpm exec vite preview --host 127.0.0.1 --port ${PORT}`,
    url: BASE_URL,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    stdout: 'pipe',
    stderr: 'pipe'
  }
});
