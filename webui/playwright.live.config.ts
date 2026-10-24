import { defineConfig } from '@playwright/test';
import mocked from './playwright.config';

export default defineConfig({
  ...mocked,
  testMatch: /live-.*\.spec\.ts/,
  outputDir: 'test-results/live',
});
