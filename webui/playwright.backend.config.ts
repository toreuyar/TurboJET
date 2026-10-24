import { defineConfig } from '@playwright/test';

/**
 * Real-backend browser suite (TJ-026). Every test starts its own tj-api
 * process, SQLite/workspace and local scripted model; no live provider or
 * external service is used. Run with `npm run e2e:backend`.
 */
export default defineConfig({
  testDir: './tests/e2e',
  testMatch: ['backend-journey.spec.ts', 'planning-journey.spec.ts'],
  timeout: 180000,
  expect: { timeout: 20000 },
  retries: 0,
  workers: 1,
  reporter: [['list'], ['html', { open: 'never' }]],
  outputDir: 'test-results/backend',
  use: {
    headless: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    downloadsPath: 'test-results/backend-downloads',
  },
});
