import { expect, test, type Page } from '@playwright/test';

/**
 * Deterministic UI E2E: the whole API is mocked at the network layer.
 * Covers: login → dashboard gates → run detail → approve & reject flows → workflows → start run.
 */

const RUNS = [
  { id: 'run_g1', project_id: null, lane_id: null, description: 'Gate run', status: 'waiting_for_gate', current_flow_id: null, gate_state: 'Approve to continue', gate_instance_id: 'gate_g1', gate_name: 'gate-a', state_version: 5, repo_url: null, base_branch: 'main', slug: null, pr_url: null, pr_number: null, created_at: '2026-09-07T10:00:00Z', updated_at: '2026-09-07T11:00:00Z', profile: 'flow-a' },
  { id: 'run_f1', project_id: null, lane_id: null, description: 'Old run', status: 'completed', current_flow_id: null, gate_state: null, gate_instance_id: null, gate_name: null, state_version: 8, repo_url: null, base_branch: 'main', slug: null, pr_url: null, pr_number: null, created_at: '2026-08-01T10:00:00Z', updated_at: '2026-08-01T11:00:00Z' },
];

const TREE = {
  run: RUNS[0],
  flows: [
    { id: 'f1', flow_name: 'flow-a', flow_number: 1, status: 'completed', branch_name: 'tj/main', started_at: '2026-09-07T10:00:00Z', completed_at: '2026-09-07T10:40:00Z',
      stages: [{ id: 's1', stage_name: 'start', status: 'completed', iteration: 0, error: null, started_at: '2026-09-07T10:00:00Z', completed_at: '2026-09-07T10:40:00Z', input_artifacts: { description: 'x' }, output_artifacts: { verdict: 'success', 'output.md': '# plan' } }] },
  ],
};

let approveCalls: { approved: boolean; reason: string | null; gate_instance_id: string; expected_state_version: number; idempotency_key: string; packet_revision_id?: string; packet_content_hash?: string; acknowledge_incomplete?: boolean; output_artifact_id?: string; output_content_hash?: string; output_stage_name?: string }[] = [];
const unexpectedRequests = new WeakMap<Page, string[]>();

test.afterEach(async ({ page }) => {
  expect(unexpectedRequests.get(page) || [], 'Every API request must be mocked; no live backend is used').toEqual([]);
});

/// Structured `question` tool fixtures (plan T-10): the question travels as a
/// `question` stage-log row (written by the tj-stage question channel); the
/// answer is POSTed as the QuestionAnswer JSON inside /input's text field (OQ-2).
const QUESTION_PAYLOAD = { question: 'Which database should we use?', options: [{ id: 'postgres', label: 'PostgreSQL' }, { id: 'sqlite', label: 'SQLite' }], multi_select: false, free_form: true };
const QUESTION_STATE = { ...QUESTION_PAYLOAD, question_id: 'q_e2e', stage_id: 's1', attempt: 0, status: 'open' };
const RUN_Q = { id: 'run_q1', project_id: null, lane_id: null, description: 'Question run', status: 'running', current_flow_id: 'f1', gate_state: null, gate_instance_id: null, gate_name: null, state_version: 2, repo_url: null, base_branch: 'main', slug: null, pr_url: null, pr_number: null, created_at: '2026-09-10T10:00:00Z', updated_at: '2026-09-10T10:05:00Z' };
const TREE_Q = {
  run: RUN_Q,
  flows: [
    { id: 'f1', flow_name: 'flow-a', flow_number: 1, status: 'running', branch_name: 'tj/main', started_at: '2026-09-10T10:00:00Z', completed_at: null,
      stages: [{ id: 's1', stage_name: 'start', status: 'waiting_for_input', iteration: 0, error: null, started_at: '2026-09-10T10:01:00Z', completed_at: null, input_artifacts: null, output_artifacts: null }] },
  ],
};
const STAGE_LOGS_Q = [
  { id: 1, run_id: 'run_q1', stage_id: 's1', stage_name: 'start', flow_name: 'flow-a', kind: 'agent_message', content: '**start asks:**\nWhich database should we use? (reply with one letter; free text welcome)', created_at: '2026-09-10T10:01:00Z' },
  { id: 2, run_id: 'run_q1', stage_id: 's1', stage_name: 'start', flow_name: 'flow-a', kind: 'question', content: JSON.stringify(QUESTION_STATE), created_at: '2026-09-10T10:01:01Z' },
];

async function mockQuestionApi(page: Page) {
  await mockApi(page);
  const inputCalls: { text: string; stage_id: string | null; attempt: number | null; question_id: string | null; idempotency_key: string | null }[] = [];
  // Registered after mockApi's generic `runs*` route, so these win per URL.
  await page.route('**/v1/pipeline/runs*', (r) => r.fulfill({ json: [...RUNS, RUN_Q] }));
  await page.route('**/v1/pipeline/runs/run_q1/tree*', (r) => r.fulfill({ json: TREE_Q }));
  await page.route('**/v1/pipeline/runs/*/stage-logs*', (r) => r.fulfill({ json: STAGE_LOGS_Q }));
  await page.route('**/v1/pipeline/runs/*/stage-logs/stream*', (r) => r.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }));
  await page.route('**/v1/pipeline/runs/*/questions*', (r) => r.fulfill({ json: { questions: [] } }));
  await page.route('**/v1/pipeline/runs/*/input*', (r) => {
    if (r.request().method() === 'POST') {
      inputCalls.push(r.request().postDataJSON());
      return r.fulfill({ json: { status: 'delivered', seq: 1 } });
    }
    return r.fulfill({ json: { bridge_active: true, waiting: true, stage_messages: [], queued: [] } });
  });
  return inputCalls;
}

const PACKET_CONTENT = {
  schema_version: 1,
  title: 'Portable requirements packet',
  problem: { statement: 'Users need a reviewable requirements packet.', source_refs: ['SRC-1'] },
  requirements: [{ id: 'REQ-1', title: 'Review packet', statement: 'Show contents and evidence.', kind: 'functional', priority: 'must', status: 'confirmed', acceptance_criteria: ['Reviewer sees evidence and unknowns.'], source_refs: ['SRC-1'] }],
  evidence: [{ id: 'EVD-1', claim: 'The start artifact records the request.', source_refs: ['SRC-1'] }],
  unknowns: [], decisions: [], contradictions: [], risks: [],
  sources: [{ id: 'SRC-1', kind: 'stage_artifact', reference: 'artifact:flow-a:start', excerpt: 'Need a packet.' }],
};
const PACKET_REVISION = {
  id: 'pr_e2e', packet_id: 'pkt_e2e', run_id: 'run_g1', revision: 1, schema_version: 1,
  content_hash: 'a'.repeat(64), completeness: 'complete', parent_revision_id: null,
  source_artifact_id: 'art_e2e', source_artifact_hash: 'b'.repeat(64), flow_name: 'flow-a', stage_name: 'artifact-a',
  change_request: null, created_by: 'system:packet-generation', created_at: '2026-09-07T10:00:00Z', content: PACKET_CONTENT,
};
const PACKET_BUNDLE = {
  run_id: 'run_g1', packet_id: 'pkt_e2e', latest: PACKET_REVISION, revisions: [PACKET_REVISION], reviews: [],
  gate_binding: { gate_instance_id: 'gate_g1', packet_id: 'pkt_e2e', revision_id: 'pr_e2e', revision: 1, content_hash: PACKET_REVISION.content_hash, completeness: 'complete' },
  allowed_source_references: ['artifact:flow-a:start'],
};

async function mockApi(page: Page) {
  approveCalls = [];
  const unexpected: string[] = [];
  unexpectedRequests.set(page, unexpected);
  await page.route('**/v1/**', (route) => {
    unexpected.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
    return route.fulfill({ status: 501, json: { error: 'Missing test fixture' } });
  });
  await page.route('**/v1/auth/login*', (r) => r.fulfill({ json: { token: 'tjt_e2e', user: { id: 'u1', username: 'e2e', display_name: null, is_admin: true } } }));
  await page.route('**/v1/me*', (r) => r.fulfill({ json: { id: 'u1', username: 'e2e', display_name: null, is_admin: true } }));
  await page.route('**/v1/readiness*', (r) => r.fulfill({ json: {
    status: 'ready', version: '1.0.0', checks: [],
    first_run: { admin_exists: true, project_count: 1, setup_required: false, next_step: 'Start a run' },
    provider: { agent_id: 'chief', provider_id: 'mock', model: 'mock-model', credential_env: 'MOCK_KEY', ready: true },
    activity_retention_days: 30,
  } }));
  await page.route('**/v1/pipeline/runs/*/diagnostics*', (r) => r.fulfill({ json: {
    run_id: 'run_g1', status: 'waiting_for_gate', state_version: 5, gate_instance_id: 'gate_g1', gate_name: 'gate-a',
    generated_at: '2026-10-22T10:00:00Z', retention_days: 30, stages: [], routes: [],
    corrections: { llm_calls: 2, output_corrections: 0, silent_turn_nudges: 0 },
    provider_usage: { calls: 2, known_usage_calls: 2, unknown_usage_calls: 0, input_tokens: 10, output_tokens: 20, cost_micros: 0 },
    provider_attempts: { total: 2, succeeded: 2, rate_limited: 0, transient: 0, credential: 0, permanent: 0 },
    budget: null, recovery: null,
  } }));
  await page.route('**/v1/pipeline/runs*', async (r) => {
    if (r.request().method() === 'POST') {
      const body = r.request().postDataJSON();
      return r.fulfill({ json: { ...RUNS[0], id: 'run_new', description: body.description, status: 'running' } });
    }
    return r.fulfill({ json: RUNS });
  });
  await page.route('**/v1/pipeline/runs/*/tree*', async (r) => {
    const url = r.request().url();
    const body = url.includes('run_new')
      ? { ...TREE, run: { ...TREE.run, id: 'run_new', description: 'e2e started run', status: 'running' } }
      : TREE;
    return r.fulfill({ json: body });
  });
  await page.route('**/v1/pipeline/runs/*/decisions*', (r) => r.fulfill({ json: [] }));
  await page.route('**/v1/pipeline/runs/*/output*', (r) => r.fulfill({ json: {
    run_id: 'run_g1', profile: 'flow-a', gate_name: 'gate-a',
    gate_binding: { gate_instance_id: 'gate_g1', run_id: 'run_g1', flow_name: 'flow-a', stage_name: 'start', artifact_id: 'art_g1', artifact_version: 1, content_hash: 'd'.repeat(64), bound_at: '2026-09-07T10:40:00Z', updated_at: '2026-09-07T10:40:00Z' },
    latest: { id: 'art_g1', run_id: 'run_g1', flow_name: 'flow-a', stage_name: 'start', attempt: 0, version: 1, content: '{"verdict":"success","content":"reviewed output"}', content_hash: 'd'.repeat(64), created_at: '2026-09-07T10:40:00Z' },
    decisions: [],
  } }));
  await page.route('**/v1/pipeline/runs/*/packet', (r) => r.fulfill({ json: PACKET_BUNDLE }));
  await page.route('**/v1/pipeline/runs/*/plan', (r) => r.fulfill({ json: {
    run_id: 'run_g1', plan_id: null, latest: null, approved: null, revisions: [], reviews: [],
    gate_binding: null, readiness: [], selections: [],
  } }));
  await page.route('**/v1/pipeline/runs/*/packet/export*', (r) => r.fulfill({ status: 200, headers: { 'Content-Type': 'application/json; charset=utf-8', 'Content-Disposition': 'attachment; filename="pkt_e2e-r1.json"' }, body: JSON.stringify(PACKET_BUNDLE) }));
  await page.route('**/v1/pipeline/runs/*/budget*', (r) => r.fulfill({ json: {
    run_id: 'run_g1', question_rounds_used: 0, question_rounds_limit: 6, question_rounds_remaining: 6,
    question_rounds_per_stage_limit: 6, model_calls_used: 2, model_calls_limit: 20, model_calls_remaining: 18,
    tool_calls_used: 0, tool_calls_limit: 20, tool_calls_remaining: 20,
    output_corrections_used: 0, output_corrections_limit: 4, output_corrections_remaining: 4,
    interaction_nudges_used: 0, interaction_nudges_limit: 4, interaction_nudges_remaining: 4,
    active_ms_used: 60000, active_ms_limit: 7200000, active_ms_remaining: 7140000,
    exhausted: false, exhausted_reason: null, updated_at: '2026-09-10T10:00:00Z',
  } }));
  await page.route('**/v1/pipeline/runs/*/recovery*', (r) => r.fulfill({ json: { recovery: null, unresolved_effects: [] } }));
  await page.route('**/v1/pipeline/runs/*/publication*', (r) => r.fulfill({ json: {
    run_id: 'run_g1', publication: null, history: [], draft_pr: null, reconciliation_required: false,
  } }));
  await page.route('**/v1/pipeline/runs/*/delivery*', (r) => r.fulfill({ json: {
    run_id: 'run_g1', releases: [], decisions: [], callbacks: [],
    integration: { configured: false, provider: null, execution_authority: 'external_ci_cd',
      limitation: 'No delivery adapter configured in this mocked UI test.' },
    execution_authority: 'external_ci_cd', note: 'external',
  } }));
  await page.route('**/v1/pipeline/runs/*/stage-logs*', (r) => r.fulfill({ json: [] }));
  await page.route('**/v1/pipeline/runs/*/questions*', (r) => r.fulfill({ json: { questions: [] } }));
  await page.route('**/v1/pipeline/runs/*/stage-logs/stream*', (r) => r.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }));
  await page.route('**/v1/pipeline/runs/*/approve*', async (r) => {
    const body = r.request().postDataJSON();
    approveCalls.push(body);
    return r.fulfill({ json: {
      id: 'decision_g1', run_id: 'run_g1', gate_instance_id: body.gate_instance_id,
      approved: body.approved, status: 'running', state_version: 6,
      disposition: body.approved ? 'approved' : 'rework', replayed: false,
    } });
  });
  await page.route('**/v1/projects*', (r) => r.fulfill({ json: [{ id: 'proj_1', name: 'E2E Project' }] }));
  await page.route('**/v1/lanes*', (r) => r.fulfill({ json: { lanes: [{ id: 'lane_1', project_id: 'proj_1', name: 'Default', smg_path: '', workflow_id: null }] } }));
  await page.route('**/v1/profiles*', (r) => r.fulfill({ json: { profiles: [
    { id: 'flow-a', label: 'Flow A', description: 'Synthetic repository-free flow', smg_path: 'fixture-a.json', maturity: 'supported', repository_free: true, gate_binding: 'packet' },
    { id: 'flow-b', label: 'Flow B', description: 'Synthetic experimental flow', smg_path: 'fixture-c.json', maturity: 'experimental', repository_free: true, gate_binding: 'none' },
    { id: 'flow-c', label: 'Repo Flow', description: 'Synthetic repository-backed flow', smg_path: 'fixture.json', maturity: 'experimental', repository_free: false, gate_binding: 'packet' },
  ] } }));
  await page.route('**/v1/workflows*', (r) => r.fulfill({
    json: {
      workflows: [{
        id: 'wf1', name: 'Synthetic workflow', smg_path: 'fixture.json',
        version: 1, revision: 1, description: 'synthetic fixture', entry: 'flow-a', exit: 'flow-b',
        flow_count: 2, gate_count: 1, stage_count: 1, maturity: 'experimental', created_at: '2026-09-09T00:00:00Z',
        content: {
          entry: 'flow-a', exit: 'flow-b', 'flow-order': ['flow-a', 'gate-a'],
          flows: {
            'flow-a': { type: 'flow', entry: 'start', stages: { start: { agent: 'fixture.json', interactive: false, routes: { success: '_next-flow' } } } },
            'gate-a': { type: 'gate', stages: {} },
          },
        },
        lanes: [],
      }],
      configured: { name: 'Synthetic workflow', version: 1 },
    },
  }));
  await page.route('**/v1/pipeline/events*', (r) => r.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }));
}

async function login(page: Page) {
  await page.goto('/');
  await page.fill('#u', 'e2e');
  await page.fill('#p', 'e2e-password');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText('Waiting on you')).toBeVisible();
}

test('login → dashboard shows the gate', async ({ page }) => {
  await mockApi(page);
  await login(page);
  await expect(page.getByText('Gate run')).toBeVisible();
  await expect(page.getByText('1 waiting')).toBeVisible();
});

test('Chief history stays private when accounts share a browser', async ({ page }) => {
  await mockApi(page);
  await page.addInitScript(() => {
    localStorage.setItem('tj-chats-v1', JSON.stringify([{ id: 'legacy', title: 'Legacy', messages: [{ who: 'you', text: 'Legacy private conversation' }] }]));
    localStorage.setItem('tj-chats-v2:u1', JSON.stringify([{ id: 'alice', title: 'Alice', messages: [{ who: 'chef', text: 'Alice private conversation from the old persona key' }] }]));
    localStorage.setItem('tj-chats-v2:bob', JSON.stringify([{ id: 'bob', title: 'Bob', messages: [{ who: 'you', text: 'Bob private conversation' }] }]));
  });
  await login(page);
  await expect(page.locator('.msg').filter({ hasText: 'Alice private conversation from the old persona key' })).toBeVisible();
  await expect(page.locator('.msg').filter({ hasText: 'Alice private conversation from the old persona key' }).locator('.who')).toHaveText('Chief');
  await expect(page.locator('.msg').filter({ hasText: 'Bob private conversation' })).toHaveCount(0);
  await expect(page.locator('.msg').filter({ hasText: 'Legacy private conversation' })).toHaveCount(0);
  await page.route('**/v1/auth/logout', (route) => route.fulfill({ json: { ok: true } }));
  await page.getByRole('button', { name: 'Sign out' }).click();
  await page.route('**/v1/me', (route) => route.fulfill({ json: { id: 'bob', username: 'bob', is_admin: false } }));
  await page.fill('#u', 'bob');
  await page.fill('#p', 'testpass');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.locator('.msg').filter({ hasText: 'Bob private conversation' })).toBeVisible();
  await expect(page.locator('.msg').filter({ hasText: 'Alice private conversation' })).toHaveCount(0);
});

test('run detail: approve posts approved=true', async ({ page }) => {
  await mockApi(page);
  await login(page);
  await page.getByText('Gate run').click();
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByText('Approve to continue')).toBeVisible();
  await expect(page.getByRole('button', { name: /^flow-a/ })).toBeVisible();
  await page.getByRole('button', { name: 'Approve' }).first().click();
  await expect.poll(() => approveCalls.length).toBe(1);
  expect(approveCalls[0].approved).toBe(true);
  expect(approveCalls[0].gate_instance_id).toBe('gate_g1');
  expect(approveCalls[0].expected_state_version).toBe(5);
  expect(approveCalls[0].idempotency_key).toBeTruthy();
});


test('generic output gate shows the exact output and posts its identity', async ({ page }) => {
  await mockApi(page);
  const run = {
    ...RUNS[0], id: 'run_c', description: 'Output run', profile: 'flow-b',
    gate_name: 'gate-b', gate_state: 'Review the produced artifact',
    gate_binding: { kind: 'artifact', gate_instance_id: 'gate_g1', flow_name: 'flow-b', stage_name: 'finish', artifact_id: 'art_review', artifact_version: 1, content_hash: 'c'.repeat(64), packet_revision_id: null, packet_content_hash: null, plan_revision_id: null, plan_content_hash: null },
  };
  const tree = {
    run,
    flows: [{ id: 'f2', flow_name: 'flow-b', flow_number: 1, status: 'completed', branch_name: null, started_at: '2026-10-22T10:00:00Z', completed_at: '2026-10-22T10:05:00Z', stages: [] }],
  };
  await page.route('**/v1/pipeline/runs*', (r) => r.fulfill({ json: [run] }));
  await page.route('**/v1/pipeline/runs/run_c/tree*', (r) => r.fulfill({ json: tree }));
  await page.route('**/v1/pipeline/runs/run_c/output*', (r) => r.fulfill({ json: {
    run_id: 'run_c', profile: 'flow-b', gate_name: 'gate-b',
    gate_binding: { kind: 'artifact', gate_instance_id: 'gate_g1', flow_name: 'flow-b', stage_name: 'finish', artifact_id: 'art_review', artifact_version: 1, content_hash: 'c'.repeat(64), packet_revision_id: null, packet_content_hash: null, plan_revision_id: null, plan_content_hash: null },
    latest: { id: 'art_review', run_id: 'run_c', flow_name: 'flow-b', stage_name: 'finish', attempt: 0, version: 1, content: '{\"deliverable\":\"Launch note v2 with security review\"}', content_hash: 'c'.repeat(64), created_at: '2026-10-22T10:05:00Z' },
    decisions: [],
  } }));
  await login(page);
  await page.getByText('Output run').click();
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByText('Exact output under review')).toBeVisible();
  await expect(page.getByText('finish · flow-b · artifact art_review')).toBeVisible();
  await page.getByRole('button', { name: 'Approve' }).first().click();
  await expect.poll(() => approveCalls.length).toBe(1);
  expect(approveCalls[0].output_artifact_id).toBe('art_review');
  expect(approveCalls[0].output_content_hash).toBe('c'.repeat(64));
  expect(approveCalls[0].output_stage_name).toBe('finish');
});

test('run detail: reject posts structured cause', async ({ page }) => {
  await mockApi(page);
  await login(page);
  await page.getByText('Gate run').click();
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await page.getByRole('button', { name: 'Reject with cause' }).first().click();
  await expect(page.getByRole('heading', { name: 'Reject with cause' })).toBeVisible();
  await page.getByRole('button', { name: /send back for rework/ }).click();
  await expect.poll(() => approveCalls.length).toBe(1);
  expect(approveCalls[0].approved).toBe(false);
  expect(approveCalls[0].reason).toBe('Scope too broad — needs splitting');
});

test('packet review: shows evidence, requires incomplete acknowledgement, revises and exports exact revision', async ({ page }) => {
  await mockApi(page);
  // Packet panels are a legacy frozen-profile compatibility surface; new
  // catalog workflows use artifact-bound gates. Keep this fixture explicit.
  const packetRun = { ...RUNS[0], profile: 'full' };
  await page.route('**/v1/pipeline/runs*', (r) => r.fulfill({ json: [packetRun] }));
  await page.route('**/v1/pipeline/runs/run_g1/tree*', (r) => r.fulfill({ json: { ...TREE, run: packetRun } }));
  const exportCalls: string[] = [];
  const revisionCalls: Record<string, unknown>[] = [];
  const incomplete = { ...PACKET_BUNDLE, latest: { ...PACKET_REVISION, completeness: 'incomplete' }, revisions: [{ ...PACKET_REVISION, completeness: 'incomplete' }], gate_binding: { ...PACKET_BUNDLE.gate_binding, completeness: 'incomplete' } };
  let currentBundle = incomplete;
  let revisedHash = PACKET_REVISION.content_hash;
  await page.route('**/v1/pipeline/runs/*/packet/export*', (r) => {
    exportCalls.push(new URL(r.request().url()).search);
    return r.fulfill({ status: 200, headers: { 'Content-Type': 'application/json; charset=utf-8', 'Content-Disposition': 'attachment; filename="pkt_e2e-r1.json"' }, body: JSON.stringify(currentBundle) });
  });
  await page.route('**/v1/pipeline/runs/*/packet/revisions', (r) => {
    const body = r.request().postDataJSON();
    revisionCalls.push(body);
    revisedHash = 'c'.repeat(64);
    const revised = {
      ...PACKET_REVISION, id: 'pr_e2e_2', revision: 2, completeness: 'incomplete',
      content_hash: revisedHash, content: body.content, parent_revision_id: 'pr_e2e', change_request: body.change_request,
    };
    currentBundle = {
      ...incomplete, latest: revised, revisions: [incomplete.revisions[0], revised],
      gate_binding: { ...incomplete.gate_binding, revision_id: revised.id, revision: 2, content_hash: revisedHash },
    };
    return r.fulfill({ status: 201, json: revised });
  });
  await page.route('**/v1/pipeline/runs/*/packet', (r) => r.fulfill({ json: currentBundle }));
  await login(page);
  await page.getByText('Gate run').click();
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByRole('heading', { name: 'Portable requirements packet' })).toBeVisible();
  await page.getByText(/Evidence \(1\)/).click();
  await expect(page.getByText(/The start artifact records the request/)).toBeVisible();
  const approve = page.getByRole('button', { name: 'Approve' }).first();
  await expect(approve).toBeDisabled();
  await page.getByRole('checkbox').check();
  await expect(approve).toBeEnabled();
  await page.getByRole('button', { name: 'Download JSON' }).click();
  await expect.poll(() => exportCalls.length).toBe(1);
  expect(exportCalls[0]).toContain('format=json');
  await page.getByRole('button', { name: 'Revise packet' }).click();
  await page.locator('#packet-json').fill(JSON.stringify({ ...PACKET_CONTENT, title: 'Portable requirements packet v2' }, null, 2));
  await page.locator('#packet-change-request').fill('clarify scope');
  await page.getByRole('button', { name: 'Save revision' }).click();
  await expect.poll(() => revisionCalls.length).toBe(1);
  expect(revisionCalls[0].change_request).toBe('clarify scope');
  await expect(page.getByText(/Revision 2 saved/)).toBeVisible();
  // The durable revision and gate binding survive a full browser refresh.
  await page.reload();
  await page.getByRole('button', { name: /Approval gate/ }).click();
  await expect(page.getByRole('heading', { name: 'Portable requirements packet v2' })).toBeVisible();
  await expect(page.getByLabel('Revision', { exact: true })).toHaveValue('pr_e2e_2');
  await page.getByRole('checkbox').check();
  await page.getByRole('button', { name: 'Approve' }).first().click();
  await expect.poll(() => approveCalls.length).toBe(1);
  expect(approveCalls[0].packet_revision_id).toBe('pr_e2e_2');
  expect(approveCalls[0].packet_content_hash).toBe(revisedHash);
  expect(approveCalls[0].acknowledge_incomplete).toBe(true);
});

test('workflows: list -> detail page renders and start run creates one', async ({ page }) => {
  await mockApi(page);
  // Detail endpoint (registered after the list wildcard so it wins for /v1/workflows/wf1)
  await page.route('**/v1/workflows/wf1', (r) => r.fulfill({ json: {
    id: 'wf1', name: 'Synthetic workflow', smg_path: 'fixture.json',
    version: 1, revision: 1, description: 'Synthetic fixture', entry: 'flow-a', exit: 'flow-b',
    flow_count: 2, gate_count: 1, stage_count: 1, maturity: 'experimental', created_at: '2026-09-08T00:00:00Z', lanes: [],
    content: {
      entry: 'flow-a', exit: 'flow-b', 'flow-order': ['flow-a', 'gate-a'],
      flows: {
        'flow-a': { type: 'flow', entry: 'start', stages: { start: { agent: 'fixture.json', interactive: true, routes: { success: '_next-flow' } } } },
        'gate-a': { type: 'gate', stages: {} },
      },
    },
  } }));
  await login(page);
  await page.getByRole('button', { name: 'Workflows' }).click();
  await expect(page.getByText('Synthetic workflow')).toBeVisible();
  await page.getByText('Synthetic workflow').click();
  await expect(page.getByRole('heading', { name: 'Synthetic workflow' })).toBeVisible();
  await expect(page.getByText('gate-a').first()).toBeVisible();
  await page.getByRole('button', { name: 'Start a run…' }).click();
  await page.fill('#wd', 'e2e started run');
  await page.getByRole('button', { name: 'Start run' }).click();
  await expect(page.locator('.crumb .here')).toHaveText('e2e started run');
});

test('run detail: structured question renders options and posts the JSON answer to /input', async ({ page }) => {
  const inputCalls = await mockQuestionApi(page);
  await login(page);
  await page.getByText('Question run').click();
  // WaitingBanner points at the agent that asked.
  await expect(page.getByText('An agent is waiting for your input')).toBeVisible();
  await page.getByRole('button', { name: /Open start/ }).click();
  await page.getByRole('button', { name: 'Chat', exact: true }).click();
  // Question card: question text + option radios (single-select).
  await expect(page.getByText('Which database should we use?', { exact: true })).toBeVisible();
  await page.getByLabel('PostgreSQL').check();
  await page.getByLabel('Free-form answer').fill('prefer hosted');
  await page.getByRole('button', { name: 'Submit answer' }).click();
  await expect.poll(() => inputCalls.length).toBe(1);
  // OQ-2: the QuestionAnswer JSON travels inside the POST /input text field.
  expect(JSON.parse(inputCalls[0].text)).toEqual({ selected: ['postgres'], text: 'prefer hosted' });
  expect(inputCalls[0].stage_id).toBe('s1');
  expect(inputCalls[0].attempt).toBe(0);
  expect(inputCalls[0].question_id).toBe('q_e2e');
  expect(inputCalls[0].idempotency_key).toBeTruthy();
});
