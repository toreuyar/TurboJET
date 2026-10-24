import { expect, test } from '@playwright/test';

/**
 * Prepared live Discovery journey (TJ-026). BLOCKED until a supported provider,
 * disposable backend, credentials and operator authorization are available.
 * Do not treat a skipped run as evidence.
 */
const enabled = process.env.TJ_E2E_LIVE_DISCOVERY === '1';

test.skip(!enabled, 'BLOCKED: TJ_E2E_LIVE_DISCOVERY=1 plus TJ_E2E_USER/TJ_E2E_PASS, a disposable backend and a supported model are required');

test('live Discovery: first admin → project → run → questions → exact packet approval', async ({ page }) => {
  const user = process.env.TJ_E2E_USER;
  const pass = process.env.TJ_E2E_PASS;
  test.skip(!user || !pass, 'BLOCKED: live credentials are not configured');

  await page.goto('/');
  await page.fill('#u', user!);
  await page.fill('#p', pass!);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText('Waiting on you')).toBeVisible();

  // The operator must attach real model/config/cost/time evidence to
  // tests/qa/scenarios/discovery-live.md before this can be marked passed.
  await page.getByRole('button', { name: 'Runs' }).click();
  await expect(page.getByRole('heading', { name: 'Runs' })).toBeVisible();
  test.info().annotations.push({ type: 'blocked-evidence', description: 'live packet approval/export and negative cases require the operator record' });
});
