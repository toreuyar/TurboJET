import { defineConfig } from 'vitest/config';

// Optional shipped-example / SDLC-domain contracts. These read repository
// workflows/ packages and use historical business names, so they are never part
// of the default asset-free unit suite.
export default defineConfig({
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/unit/setup.ts'],
    include: ['tests/examples/**/*.test.{ts,tsx}'],
  },
});
