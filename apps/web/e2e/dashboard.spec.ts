// hq-fe-build.7 — smoke test that the production SvelteKit bundle boots and the
// shell renders without runtime errors. Real API-bound coverage lands as the
// individual views ship their data-test ids; this seeds the harness so future
// specs can extend it.

import { expect, test } from '@playwright/test';

test('home page renders without console errors', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });

  await page.goto('/');

  // Wait for the SvelteKit hydration to settle so error events from the client
  // bundle have time to surface.
  await page.waitForLoadState('networkidle');

  // The shell must respond with a 200 + render something — we deliberately stay
  // off specific copy so this does not break when the dashboard layout changes.
  await expect(page.locator('body')).toBeVisible();

  expect(errors, errors.join('\n')).toEqual([]);
});
