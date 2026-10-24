import { chromium } from '@playwright/test';

try {
  const browser = await chromium.launch({ headless: true });
  await browser.close();
} catch (error) {
  console.error('BLOCKED: Chromium cannot launch. Install the locked browser and OS dependencies with:');
  console.error('  npx playwright install --with-deps chromium');
  console.error(error.message);
  process.exitCode = 2;
}
