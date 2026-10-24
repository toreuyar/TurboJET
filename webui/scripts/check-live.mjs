const required = ['TJ_E2E_USER', 'TJ_E2E_PASS'];
const discovery = process.env.TJ_E2E_LIVE_DISCOVERY === '1';
if (!discovery) console.error('BLOCKED: live Discovery journey is skipped (set TJ_E2E_LIVE_DISCOVERY=1 with a supported model and disposable backend).');
const missing = required.filter((key) => !process.env[key]);
if (missing.length) {
  console.error(`BLOCKED: live smoke requires explicit ${missing.join(', ')}.`);
  process.exitCode = 2;
} else {
  const base = process.env.TJ_E2E_API_URL || 'http://127.0.0.1:8100';
  try {
    const response = await fetch(`${base}/api/health`, { signal: AbortSignal.timeout(2000) });
    if (!response.ok) throw new Error(`health returned ${response.status}`);
  } catch (error) {
    console.error(`BLOCKED: live smoke API is unavailable at ${base}: ${error.message}`);
    process.exitCode = 2;
  }
}
