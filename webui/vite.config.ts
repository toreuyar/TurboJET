import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { viteSingleFile } from 'vite-plugin-singlefile';

export default defineConfig(({ mode }) => ({
  plugins: [react(), ...(mode === 'production' ? [viteSingleFile()] : [])],
  server: {
    port: 5173,
    proxy: {
      '/v1': { target: process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100', changeOrigin: true },
      '/api': { target: process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100', changeOrigin: true },
    },
  },
  preview: {
    port: 8099,
    proxy: {
      '/v1': { target: process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100', changeOrigin: true },
      '/api': { target: process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100', changeOrigin: true },
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/unit/setup.ts'],
    include: ['tests/unit/**/*.test.{ts,tsx}'],
  },
}));
