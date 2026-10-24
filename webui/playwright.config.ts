import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: 'webui.spec.ts',
  timeout: 30000,
  retries: 0,
  reporter: [['list'], ['html', { open: 'never' }]],
  outputDir: 'test-results/mocked',
  use: {
    baseURL: 'http://127.0.0.1:8099',
    headless: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer: {
    command: 'npx vite preview --port 8099 --strictPort --host 127.0.0.1',
    port: 8099,
    reuseExistingServer: false,
    timeout: 20000,
  },
});
