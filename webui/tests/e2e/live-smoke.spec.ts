import { expect, test } from '@playwright/test';

/**
 * Live smoke against the real TurboJet on :8100 (via the preview proxy).
 * Explicit opt-in: npm run e2e:live. Missing prerequisites fail as BLOCKED.
 */

async function healthy(): Promise<boolean> {
  try {
    const r = await fetch(`${process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100'}/api/health`, { signal: AbortSignal.timeout(2000) });
    return r.ok;
  } catch {
    return false;
  }
}

test('live: login → dashboard renders real runs', async ({ page }) => {
  expect(await healthy(), 'BLOCKED: TurboJet test server not running').toBe(true);
  const user = process.env.TJ_E2E_USER;
  const pass = process.env.TJ_E2E_PASS;
  if (!user || !pass) throw new Error('BLOCKED: TJ_E2E_USER and TJ_E2E_PASS are required');

  await page.goto('/');
  await page.fill('#u', user);
  await page.fill('#p', pass);
  await page.getByRole('button', { name: 'Sign in' }).click();

  // Either the dashboard renders (authed) or login failed with a visible error
  const dash = page.getByText('Waiting on you');
  const err = page.locator('p[style*="--fail"]');
  await Promise.race([expect(dash).toBeVisible({ timeout: 8000 }), expect(err).toBeVisible({ timeout: 8000 })]);
  await expect(dash).toBeVisible();

  // Runs view loads real data (may be empty — the view itself must render)
  await page.getByRole('button', { name: 'Runs' }).click();
  await expect(page.getByRole('heading', { name: 'Runs' })).toBeVisible();
});
