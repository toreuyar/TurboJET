import { expect, test, type Page } from '@playwright/test';
import { BackendHarness } from './backend-harness';

/**
 * TJ-032 real-backend browser planning journey.
 *
 * The actual API/Manager/Flow/Stage chain runs a runtime-synthesized neutral
 * planning package (approved packet → survey → outline → sketch → align → plan
 * → deterministic check → human gate) against a local scripted model.
 * Rejected evidence performs one bounded rework, a reviewer edit makes an
 * earlier plan approval stale, and a ready task is selected for Engineering.
 *
 * This is local scripted evidence, not external pilot validation. No live
 * provider, real team feedback, repository publication or release is claimed.
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

type RunState = { id: string; status: string; gate_name?: string | null; gate_instance_id?: string | null; state_version: number };

async function runState(page: Page, runId: string): Promise<RunState> {
  return await api(page, `/v1/pipeline/runs/${runId}`) as unknown as RunState;
}

async function waitForGate(page: Page, runId: string, gateName: string) {
  await expect.poll(async () => {
    const state = await runState(page, runId);
    return `${state.status}:${state.gate_name ?? ''}`;
  }, { timeout: 45000 }).toBe(`waiting_for_gate:${gateName}`);
  return await runState(page, runId);
}

async function openRun(page: Page, runId: string) {
  await page.goto(`${harness.baseUrl}/#view=run&run=${encodeURIComponent(runId)}`);
  await page.reload();
  await expect(page.locator('.crumb .here')).toBeVisible({ timeout: 20000 });
}

async function registerFirstAdmin(page: Page) {
  await page.goto(harness.baseUrl);
  await page.getByRole('button', { name: 'First run? Create account' }).click();
  await page.fill('#u', 'planning-admin');
  await page.fill('#p', 'planning-password');
  await page.getByRole('button', { name: 'Create first account' }).click();
  await expect(page.getByRole('heading', { name: 'Set up TurboJet' })).toBeVisible({ timeout: 20000 });
}

async function createFullProject(page: Page) {
  const response = await page.request.post(`${harness.baseUrl}/v1/projects`, {
    headers: { Authorization: `Bearer ${await token(page)}`, 'Content-Type': 'application/json' },
    data: { name: 'Planning Pilot Team', catalog_id: 'neutral-planning' },
  });
  expect(response.ok(), `create project: ${response.status()} ${await response.text()}`).toBeTruthy();
  await page.reload();
  await expect(page.locator('#new-run-card')).toBeVisible({ timeout: 20000 });
}

async function startRun(page: Page, description: string) {
  const task = page.getByPlaceholder(/Describe the task/);
  await expect(task).toBeVisible({ timeout: 20000 });
  await expect.poll(async () => page.getByLabel('Workflow').locator('option').evaluateAll((options) =>
    options.filter((option) => (option as HTMLOptionElement).value !== '').length), { timeout: 20000 })
    .toBeGreaterThan(0);
  await task.fill(description);
  await expect(page.getByRole('button', { name: 'Run', exact: true })).toBeEnabled({ timeout: 20000 });
  await page.getByRole('button', { name: 'Run', exact: true }).click();
  await expect(page.locator('.crumb .here')).toContainText(description, { timeout: 20000 });
}

async function packet(page: Page, runId: string) {
  return await api(page, `/v1/pipeline/runs/${runId}/packet`) as {
    latest: { id: string; content_hash: string } | null;
  };
}

async function plan(page: Page, runId: string) {
  return await api(page, `/v1/pipeline/runs/${runId}/plan`) as {
    latest: {
      id: string; revision: number; content_hash: string;
      content: { tasks?: unknown[]; [key: string]: unknown };
      validation?: { valid: boolean; task_count: number; [key: string]: unknown };
    } | null;
    approved: { id: string } | null;
    gate_binding: { revision_id: string; content_hash: string } | null;
    selections: { task_id: string; plan_revision_id: string; plan_content_hash: string }[];
  };
}

async function openGate(page: Page) {
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByRole('heading', { name: 'Approval gate' })).toBeVisible({ timeout: 20000 });
}

async function approve(page: Page) {
  // The following gate/status wait is the acceptance check: the UI must send
  // the exact bound plan+packet identities, not a helper-constructed payload.
  await page.getByRole('button', { name: 'Approve', exact: true }).click();
}

test('browser planning journey: packet → validated plan → bounded rework → stale edit → exact approval → ready task handoff', async ({ page }) => {
  harness = await BackendHarness.start({ packagePath: 'config/workflows/planning' });
  await harness.setMode('neutral_planning');
  await registerFirstAdmin(page);
  await createFullProject(page);
  await startRun(page, 'Plan online cancellation work');

  // The run id is in the URL hash after the UI creates it.
  const hash = await page.evaluate(() => window.location.hash);
  const runId = /run=([^&]+)/.exec(hash)?.[1];
  expect(runId, 'run id present in navigation').toBeTruthy();
  const run = decodeURIComponent(runId!);

  await waitForGate(page, run, 'collect-gate');
  const packetBundle = await packet(page, run);
  expect(packetBundle.latest).not.toBeNull();
  await openRun(page, run);
  await openGate(page);
  await expect(page.getByRole('heading', { name: 'Requirements packet' })).toBeVisible();
  await approve(page);

  const planningGate = await waitForGate(page, run, 'shape-gate');
  const before = await plan(page, run);
  expect(before.latest?.validation?.valid).toBe(true);
  expect(before.latest?.content?.tasks?.length).toBe(4);
  expect(before.gate_binding?.revision_id).toBe(before.latest?.id);

  // Rejected evidence sends Planning back for one bounded rework. The same
  // graph executes all required stages again and returns to a new gate.
  await openRun(page, run);
  await openGate(page);
  await expect(page.getByRole('heading', { name: 'Structured task plan' })).toBeVisible();
  await expect(page.getByText(/Deterministic validation precedes the human gate/)).toBeVisible();
  await expect(page.getByText(/Blocked: dependency/).first()).toBeVisible();
  await page.getByRole('button', { name: 'Reject with cause' }).click();
  await page.getByRole('button', { name: /send back for rework/ }).click();
  const reworked = await waitForGate(page, run, 'shape-gate');
  expect(reworked.state_version).toBeGreaterThan(planningGate.state_version);

  // Reviewer edit creates a new immutable revision and rebinds the gate.
  await openRun(page, run);
  await openGate(page);
  const oldPlan = await plan(page, run);
  const oldRevisionId = oldPlan.latest!.id;
  const oldHash = oldPlan.latest!.content_hash;
  await page.getByRole('button', { name: 'Revise plan' }).click();
  const editor = page.getByLabel('Plan JSON (task-plan schema v1)');
  const content = JSON.parse(await editor.inputValue()) as Record<string, unknown>;
  content.title = 'Planning pilot — browser review revision';
  content.summary = 'Refund-preview verification is explicit and traceable to the approved packet.';
  await editor.fill(JSON.stringify(content, null, 2));
  await page.getByLabel('Reason for the revision').fill('address measurable evidence');
  await page.getByRole('button', { name: 'Save plan revision' }).click();
  await expect(page.getByText(/any earlier plan approval is stale/)).toBeVisible({ timeout: 20000 });

  const refreshed = await runState(page, run);
  const revised = await plan(page, run);
  expect(revised.latest?.revision).toBe(2);
  expect(revised.latest?.id).not.toBe(oldRevisionId);

  const staleResponse = await page.request.post(`${harness.baseUrl}/v1/pipeline/runs/${run}/approve`, {
    headers: { Authorization: `Bearer ${await token(page)}`, 'Content-Type': 'application/json' },
    data: {
      approved: true,
      gate_instance_id: refreshed.gate_instance_id,
      expected_state_version: refreshed.state_version,
      idempotency_key: `stale-browser-plan-${Date.now()}`,
      packet_revision_id: packetBundle.latest!.id,
      packet_content_hash: packetBundle.latest!.content_hash,
      plan_revision_id: oldRevisionId,
      plan_content_hash: oldHash,
    },
  });
  expect(staleResponse.status(), 'stale plan approval must conflict').toBe(409);

  await openRun(page, run);
  await openGate(page);
  await approve(page);
  await expect.poll(async () => (await runState(page, run)).status, { timeout: 30000 }).toBe('completed');

  // The approved plan remains accessible in the Plan tab. Only a ready task
  // can be handed to Engineering, and the selection retains plan/packet identity.
  await openRun(page, run);
  await page.getByRole('button', { name: 'Plan', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Structured task plan' })).toBeVisible({ timeout: 20000 });
  const selectButtons = page.getByRole('button', { name: 'Select task' });
  await expect(selectButtons.first()).toBeEnabled({ timeout: 20000 });
  await expect(selectButtons.nth(1)).toBeDisabled();
  await selectButtons.first().click();
  await expect(page.getByText(/criteria and provenance are retained in the handoff payload/)).toBeVisible({ timeout: 20000 });

  const finalPlan = await plan(page, run);
  expect(finalPlan.approved?.id).toBe(finalPlan.latest?.id);
  expect(finalPlan.selections).toHaveLength(1);
  expect(finalPlan.selections[0].task_id).toBe('T-1');
  expect(finalPlan.selections[0].plan_revision_id).toBe(finalPlan.latest?.id);
  expect(finalPlan.selections[0].plan_content_hash).toBe(finalPlan.latest?.content_hash);

  // External team validation is explicitly not claimed by this scripted run.
  expect(await page.getByText(/Task planning is experimental/).count()).toBeGreaterThan(0);
});
