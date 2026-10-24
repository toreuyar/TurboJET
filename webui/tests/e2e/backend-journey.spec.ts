import { expect, test, type Page } from '@playwright/test';
import { BackendHarness } from './backend-harness';

/**
 * TJ-026 real-backend Discovery browser journey.
 *
 * Uses the actual Axum API, Manager, Flow/Stage/runner, SQLite, artifact
 * workspace and the built UI. The model is a local scripted OpenAI-compatible
 * fixture (`scripts/discovery_scripted_model.py`), never a live provider.
 *
 * LOCAL SCRIPTED SUCCESS IS NOT LIVE SUPPORTED-MODEL PROOF. Live model/config/
 * cost/time evidence remains BLOCKED until an operator runs the labelled live
 * scenario with a supported provider.
 */

let harness: BackendHarness;

test.afterEach(async () => {
  await harness?.stop();
});

async function token(page: Page): Promise<string> {
  const value = await page.evaluate(() => localStorage.getItem('tj-token'));
  if (!value) throw new Error('browser session has no token');
  return value;
}

async function api(page: Page, pathname: string): Promise<Record<string, unknown>> {
  const response = await page.request.get(`${harness.baseUrl}${pathname}`, {
    headers: { Authorization: `Bearer ${await token(page)}` },
  });
  expect(response.ok(), `${pathname}: ${response.status()}`).toBeTruthy();
  return response.json();
}

async function registerFirstAdmin(page: Page) {
  await page.goto(harness.baseUrl);
  await page.getByRole('button', { name: 'First run? Create account' }).click();
  await page.fill('#u', 'browser-admin');
  await page.fill('#p', 'browser-password');
  await page.getByRole('button', { name: 'Create first account' }).click();
  await expect(page.getByRole('heading', { name: 'Set up TurboJet' })).toBeVisible({ timeout: 20000 });
}

async function createDiscoveryProject(page: Page) {
  const form = page.locator('form.setup-form');
  await form.getByLabel('Project name').fill('Browser Discovery');
  await form.getByLabel('Workflow').selectOption('discovery');
  await form.getByRole('button', { name: 'Create project' }).click();
  await expect(page.getByLabel('Project name')).toHaveCount(0, { timeout: 20000 });
  await expect(page.locator('#new-run-card')).toBeVisible();
}

async function startRun(page: Page, description: string) {
  const task = page.getByPlaceholder(/Describe the task/);
  await expect(task).toBeVisible({ timeout: 20000 });
  // Wait until the project lane (workflow) is selectable.
  await expect.poll(async () => page.locator('#new-run-card select').nth(1).locator('option').count(), { timeout: 20000 })
    .toBeGreaterThan(0);
  await task.fill(description);
  await page.getByRole('button', { name: 'Run', exact: true }).click();
  await expect(page.locator('.crumb .here')).toContainText(description, { timeout: 20000 });
}

async function runState(page: Page, runId: string): Promise<{ status: string; id: string }> {
  const value = await api(page, `/v1/pipeline/runs/${runId}`) as { status: string; id: string };
  return value;
}

async function openRun(page: Page, runId: string) {
  await page.goto(`${harness.baseUrl}/#view=run&run=${encodeURIComponent(runId)}`);
  await page.reload();
  await expect(page.locator('.crumb .here')).toBeVisible({ timeout: 20000 });
}

type OpenQuestion = { id: string; stage_name: string; stage_id: string; attempt: number };

async function openQuestion(page: Page, runId: string): Promise<OpenQuestion | null> {
  const questions = await api(page, `/v1/pipeline/runs/${runId}/questions?status=open`) as {
    questions: OpenQuestion[];
  };
  return questions.questions[0] ?? null;
}

async function waitForOpenQuestion(page: Page, runId: string): Promise<OpenQuestion> {
  let found: OpenQuestion | null = null;
  await expect.poll(async () => {
    found = await openQuestion(page, runId);
    return found?.id ?? null;
  }, { timeout: 30000 }).not.toBeNull();
  return found!;
}

/// Browser submission for the first question: proves the real UI card, free
/// form, and identity-bearing POST work end to end.
async function answerViaUi(page: Page, runId: string, question: OpenQuestion, answer: string) {
  const stageRow = page.locator('button.rowbtn').filter({ hasText: new RegExp(`^${question.stage_name}`) }).last();
  if (await stageRow.count() === 0) {
    await openRun(page, runId);
  }
  await stageRow.click();
  const freeForm = page.getByLabel('Free-form answer');
  await expect(freeForm).toBeVisible({ timeout: 20000 });
  await freeForm.fill(answer);
  await page.getByRole('button', { name: 'Submit answer' }).click();
  await expect(page.getByText('answer sent')).toBeVisible({ timeout: 20000 });
}

/// Subsequent answers use the same authenticated API the UI calls. This keeps
/// multi-round negative journeys deterministic while still exercising the real
/// durable question identity and backend acceptance.
async function answerOpenQuestion(page: Page, runId: string, answer: string): Promise<boolean> {
  const question = await openQuestion(page, runId);
  if (!question) return false;
  const response = await page.request.post(`${harness.baseUrl}/v1/pipeline/runs/${runId}/input`, {
    headers: {
      Authorization: `Bearer ${await token(page)}`,
      'Content-Type': 'application/json',
    },
    data: {
      text: JSON.stringify({ selected: [], text: answer }),
      stage_id: question.stage_id,
      attempt: question.attempt,
      question_id: question.id,
      idempotency_key: `e2e-${question.id}-${Date.now()}`,
    },
  });
  expect(response.ok(), `answer ${question.stage_name}: ${response.status()} ${await response.text()}`).toBeTruthy();
  return true;
}

async function advanceUntil(page: Page, runId: string, wanted: string, answer: string, maxRounds = 12) {
  for (let round = 0; round < maxRounds; round += 1) {
    const state = await runState(page, runId);
    if (state.status === wanted) return state;
    if (['failed', 'cancelled'].includes(state.status)) return state;
    if (await answerOpenQuestion(page, runId, answer)) {
      await page.waitForTimeout(200);
      continue;
    }
    await page.waitForTimeout(300);
  }
  return runState(page, runId);
}

async function approveExactPacket(page: Page) {
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByRole('button', { name: 'Download JSON' })).toBeVisible({ timeout: 20000 });
  const downloadPromise = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download JSON' }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toMatch(/\.json$/);
  await page.getByRole('button', { name: 'Approve', exact: true }).first().click();
  // The durable decision is authoritative; Discovery may complete immediately
  // after approval, so the transient UI banner is not used as evidence.
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  await expect.poll(async () => {
    const decisions = await api(page, `/v1/pipeline/runs/${runId}/decisions`) as {
      approved: boolean; packet_revision_id: string | null; packet_content_hash: string | null;
    }[];
    return decisions.some((decision) => decision.approved && Boolean(decision.packet_revision_id) && Boolean(decision.packet_content_hash));
  }, { timeout: 30000 }).toBeTruthy();
}

test('happy path: first admin → project → Chief/direct start → questions → exact packet download/approval', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('happy');
  await registerFirstAdmin(page);
  const startup = await page.evaluate(() => {
    const entry = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined;
    return {
      domContentLoadedMs: entry ? Math.round(entry.domContentLoadedEventEnd) : null,
      loadMs: entry ? Math.round(entry.loadEventEnd) : null,
      transferBytes: entry?.transferSize ?? null,
    };
  });
  console.log('UI startup metrics', JSON.stringify(startup));
  // Narrow supported viewport: the setup checklist and native controls stay
  // readable/actionable before the journey proceeds.
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('heading', { name: 'Set up TurboJet' })).toBeVisible();
  await expect(page.getByLabel('Project name')).toBeVisible();
  await page.setViewportSize({ width: 1280, height: 900 });
  await createDiscoveryProject(page);
  await startRun(page, 'Support online cancellation for solo travellers with a refund preview');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  expect(runId).toBeTruthy();

  const firstQuestion = await waitForOpenQuestion(page, runId);
  await answerViaUi(page, runId, firstQuestion, 'A traveller should cancel without contacting support and see the refund before confirming.');
  const outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'A traveller should cancel without contacting support and see the refund before confirming.');
  expect(outcome.status).toBe('waiting_for_gate');

  const packet = await api(page, `/v1/pipeline/runs/${runId}/packet`) as {
    latest: { revision: number; content_hash: string; completeness: string };
    gate_binding: { revision_id: string; content_hash: string };
  };
  expect(packet.latest.completeness).toBe('complete');
  await approveExactPacket(page);
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 30000 }).toBe('completed');

  // Support export is credential-safe and tied to the same run, not a transcript.
  await openRun(page, runId);
  const supportPromise = page.waitForEvent('download');
  await page.getByRole('button', { name: /Download credential-safe support export/ }).click();
  const support = await supportPromise;
  expect(support.suggestedFilename()).toContain('turbojet-support-');
});

test('contradictory packet is visible but approval is blocked', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('contradiction');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Add cancellation but promise an unconditional full refund');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  const outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'A traveller wants a full refund immediately even inside 48 hours.');
  expect(outcome.status).toBe('waiting_for_gate');
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByText(/contradiction/i).first()).toBeVisible({ timeout: 20000 });
  await expect(page.getByRole('button', { name: 'Approve', exact: true }).first()).toBeDisabled();
  expect((await runState(page, runId)).status).toBe('waiting_for_gate');
});

test('rejection creates rework and the next revision can be approved', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('reject_once');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Cancellation flow with a measurable outcome');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  const outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'The traveller must see the refund amount before confirming.');
  expect(outcome.status).toBe('waiting_for_gate');
  const tree = await api(page, `/v1/pipeline/runs/${runId}/tree`) as {
    flows: { stages: { stage_name: string; iteration: number }[] }[];
  };
  const intakes = tree.flows.flatMap((flow) => flow.stages).filter((stage) => stage.stage_name === 'intake');
  expect(intakes.length).toBeGreaterThan(1); // first intake, then bounded rework after reject
  await approveExactPacket(page);
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 30000 }).toBe('completed');
});

test('reconnect and delayed reply preserve the durable open question', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('happy');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Reconnect during a durable question');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';

  // Wait for the first durable question, then reload the browser and pause.
  const question = await waitForOpenQuestion(page, runId);
  await page.reload();
  await expect(page.locator('.crumb .here')).toBeVisible({ timeout: 20000 });
  await page.waitForTimeout(1500);
  await answerViaUi(page, runId, question, 'The answer survives a browser reconnect and a delayed reply.');
  const outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'The answer survives a browser reconnect and a delayed reply.');
  expect(outcome.status).toBe('waiting_for_gate');
});

test('cancel while a question is pending stops execution without another model call', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('happy');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Cancel a Discovery run while it waits for input');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  await expect.poll(async () => {
    const value = await api(page, `/v1/pipeline/runs/${runId}/questions?status=open`) as { questions: unknown[] };
    return value.questions.length;
  }, { timeout: 30000 }).toBeGreaterThan(0);
  const before = await (await fetch(`${harness.modelUrl}/__state`)).json() as { requests: number };

  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await page.getByRole('button', { name: 'Cancel the run' }).click();
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 30000 }).toBe('cancelled');
  await page.waitForTimeout(1000);
  const after = await (await fetch(`${harness.modelUrl}/__state`)).json() as { requests: number };
  expect(after.requests).toBe(before.requests);
  const questions = await api(page, `/v1/pipeline/runs/${runId}/questions?status=open`) as { questions: unknown[] };
  expect(questions.questions).toHaveLength(0);
});

test('restart marks the run interrupted and explicit retry reaches a fresh gate', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('happy');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Restart while a durable question is open');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  await expect.poll(async () => {
    const value = await api(page, `/v1/pipeline/runs/${runId}/questions?status=open`) as { questions: unknown[] };
    return value.questions.length;
  }, { timeout: 30000 }).toBeGreaterThan(0);

  await harness.restart();
  await page.reload();
  await expect(page.getByText(/Run interrupted by a restart/)).toBeVisible({ timeout: 30000 });
  await page.getByRole('button', { name: 'Retry' }).click();
  await page.getByRole('button', { name: 'Start new attempt' }).click();
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 30000 }).toBe('running');
  const outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'A retry starts a new attempt from durable state.');
  expect(outcome.status).toBe('waiting_for_gate');
});

test('transient provider failure retries transport and completes; credential failure fails closed', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('provider_fail_once');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'A transient provider outage should not replay tools');
  let runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  let outcome = await advanceUntil(page, runId, 'waiting_for_gate', 'Transient failures are retried at transport only.');
  expect(outcome.status).toBe('waiting_for_gate');
  let diag = await api(page, `/v1/pipeline/runs/${runId}/diagnostics`) as { provider_attempts: { transient: number; credential: number } };
  const modelState = await (await fetch(`${harness.modelUrl}/__state`)).json() as { requests: number };
  expect(modelState.requests).toBeGreaterThan(1);
  expect(diag.provider_attempts.transient).toBeGreaterThan(0);
  expect(diag.provider_attempts.credential).toBe(0);

  // New run with a permanently bad credential: visible failure, no retry loop.
  await harness.setMode('provider_credential');
  await page.goto(`${harness.baseUrl}/#view=dashboard`);
  await startRun(page, 'Credential failure should be actionable and not retried');
  runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 30000 }).toBe('failed');
  diag = await api(page, `/v1/pipeline/runs/${runId}/diagnostics`) as { provider_attempts: { credential: number; permanent: number; total: number } };
  expect(diag.provider_attempts.total).toBeGreaterThan(0);
  expect(diag.provider_attempts.credential).toBeGreaterThan(0);
  await expect(page.getByText('Failed').first()).toBeVisible();
});

test('question budget exhaustion fails visibly and durably', async ({ page }) => {
  harness = await BackendHarness.start();
  await harness.setMode('endless_questions');
  await registerFirstAdmin(page);
  await createDiscoveryProject(page);
  await startRun(page, 'Endless questions must stop at the configured budget');
  const runId = new URL(page.url()).hash.match(/run=([^&]+)/)?.[1] || '';
  for (let round = 0; round < 16; round += 1) {
    const state = await runState(page, runId);
    if (state.status === 'failed') break;
    if (!(await answerOpenQuestion(page, runId, `Answer ${round + 1}: scope is the online cancellation flow.`))) {
      await page.waitForTimeout(500);
    }
  }
  await expect.poll(async () => (await runState(page, runId)).status, { timeout: 60000 }).toBe('failed');
  const budget = await api(page, `/v1/pipeline/runs/${runId}/budget`) as { exhausted: boolean; exhausted_reason: string | null };
  expect(budget.exhausted).toBeTruthy();
  expect(budget.exhausted_reason).toContain('question_rounds');
  await openRun(page, runId);
  await expect(page.getByText(/Budget exhausted: question_rounds/)).toBeVisible({ timeout: 20000 });
});
