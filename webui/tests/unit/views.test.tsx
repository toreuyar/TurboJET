import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RunsList, RunDetail } from '../../src/views/Runs';
import { Workflows, WorkflowDetailPage } from '../../src/views/Workflows';
import { api } from '../../src/api';
import type { Nav } from '../../src/App';
import type { RunTree, TjRun } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    runs: vi.fn(),
    tree: vi.fn(),
    decisions: vi.fn(),
    approve: vi.fn(),
    cancel: vi.fn(),
    budget: vi.fn(),
    diagnostics: vi.fn(),
    publication: vi.fn(),
    delivery: vi.fn(),
    readiness: vi.fn(),
    projects: vi.fn(),
    downloadSupportExport: vi.fn(),
    packet: vi.fn(),
    plan: vi.fn(),
    savePlanRevision: vi.fn(),
    downloadPlan: vi.fn(),
    selectPlanTask: vi.fn(),
    planTasks: vi.fn(),
    savePacketRevision: vi.fn(),
    downloadPacket: vi.fn(),
    recovery: vi.fn(),
    retry: vi.fn(),
    createRun: vi.fn(),
    workflows: vi.fn(),
    workflow: vi.fn(),
    lanes: vi.fn(),
    chat: vi.fn(),
  },
  subscribeEvents: vi.fn(() => () => {}),
  getToken: vi.fn(() => 'tok'),
  setToken: vi.fn(),
}));

class ROStub { observe() {} unobserve() {} disconnect() {} }
if (!('ResizeObserver' in globalThis)) (globalThis as Record<string, unknown>).ResizeObserver = ROStub;

const nav: Nav = {
  view: 'runs', projectId: null, runId: null, wfId: null,
  setView: vi.fn(), openRun: vi.fn(), openWorkflow: vi.fn(), setProject: vi.fn(),
};

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

const runs: TjRun[] = [
  { id: 'r1', project_id: null, lane_id: null, description: 'Billing rewrite', status: 'waiting_for_gate', current_flow_id: null, gate_state: 'Approve the plan', gate_instance_id: 'gate_r1', gate_name: 'gate-a', state_version: 3, repo_url: null, base_branch: 'main', slug: null, pr_url: null, pr_number: null, created_at: '2026-09-07T10:00:00Z', updated_at: '2026-09-07T11:00:00Z' },
  { id: 'r2', project_id: null, lane_id: null, description: 'Old feature', status: 'completed', current_flow_id: null, gate_state: null, gate_instance_id: null, gate_name: null, state_version: 9, repo_url: null, base_branch: 'main', slug: null, pr_url: 'https://git/pr/1', pr_number: 1, created_at: '2026-08-01T10:00:00Z', updated_at: '2026-08-02T10:00:00Z' },
  { id: 'r3', project_id: null, lane_id: null, description: 'Broken thing', status: 'failed', current_flow_id: null, gate_state: null, gate_instance_id: null, gate_name: null, state_version: 4, repo_url: null, base_branch: 'main', slug: null, pr_url: null, pr_number: null, created_at: '2026-08-05T10:00:00Z', updated_at: '2026-08-05T11:00:00Z' },
];

describe('RunsList', () => {
  beforeEach(() => vi.clearAllMocks());

  it('lists runs and filters by status', async () => {
    vi.mocked(api.runs).mockResolvedValue(runs);
    withClient(<RunsList nav={nav} />);
    expect(await screen.findByText('Billing rewrite')).toBeInTheDocument();
    expect(screen.getByText('Old feature')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Failed' }));
    expect(screen.getByText('Broken thing')).toBeInTheDocument();
    expect(screen.queryByText('Old feature')).not.toBeInTheDocument();
  });

  it('searches by description', async () => {
    vi.mocked(api.runs).mockResolvedValue(runs);
    withClient(<RunsList nav={nav} />);
    await screen.findByText('Billing rewrite');
    fireEvent.change(screen.getByPlaceholderText('Search…'), { target: { value: 'billing' } });
    expect(screen.getByText('Billing rewrite')).toBeInTheDocument();
    expect(screen.queryByText('Broken thing')).not.toBeInTheDocument();
  });
});

describe('RunDetail gate flow', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.budget).mockResolvedValue({
      run_id: 'r1', question_rounds_used: 1, question_rounds_limit: 6, question_rounds_remaining: 5,
      question_rounds_per_stage_limit: 6, model_calls_used: 2, model_calls_limit: 20, model_calls_remaining: 18,
      tool_calls_used: 0, tool_calls_limit: 20, tool_calls_remaining: 20,
      output_corrections_used: 0, output_corrections_limit: 4, output_corrections_remaining: 4,
      interaction_nudges_used: 0, interaction_nudges_limit: 4, interaction_nudges_remaining: 4,
      active_ms_used: 120000, active_ms_limit: 7200000, active_ms_remaining: 7080000,
      cost_micros_used: 0, cost_micros_limit: 0, cost_micros_remaining: 0, unknown_usage_calls: 0,
      exhausted: false, exhausted_reason: null, updated_at: '2026-09-07T10:00:00Z',
    });
    vi.mocked(api.recovery).mockResolvedValue({ recovery: null, unresolved_effects: [] });
    vi.mocked(api.publication).mockResolvedValue({ run_id: 'r1', publication: null, history: [], draft_pr: null, reconciliation_required: false });
    vi.mocked(api.delivery).mockResolvedValue({ run_id: 'r1', releases: [], decisions: [], callbacks: [],
      integration: { configured: false, provider: null, execution_authority: 'external_ci_cd', limitation: 'not configured' },
      execution_authority: 'external_ci_cd', note: 'external' });
    vi.mocked(api.diagnostics).mockResolvedValue({
      run_id: 'r1', status: 'running', state_version: 3, gate_instance_id: 'gate_r1', gate_name: 'gate-a',
      generated_at: '2026-09-07T10:00:00Z', retention_days: 30, stages: [], routes: [],
      corrections: { llm_calls: 2, output_corrections: 0, silent_turn_nudges: 0 },
      provider_usage: { calls: 2, known_usage_calls: 2, unknown_usage_calls: 0, input_tokens: 10, output_tokens: 20, cost_micros: 0 },
      provider_attempts: { total: 2, succeeded: 2, rate_limited: 0, transient: 0, credential: 0, permanent: 0 },
      budget: null, recovery: null,
    });
    vi.mocked(api.packet).mockResolvedValue({
      run_id: 'r1', packet_id: null, latest: null, revisions: [], reviews: [],
      gate_binding: null, allowed_source_references: [],
    });
    vi.mocked(api.plan).mockResolvedValue({
      run_id: 'r1', plan_id: null, latest: null, approved: null, revisions: [], reviews: [],
      gate_binding: null, readiness: [], selections: [],
    });
  });

  const tree: RunTree = {
    run: runs[0],
    flows: [{ id: 'f1', flow_name: 'flow-a', flow_number: 1, status: 'completed', branch_name: 'tj/x', started_at: '2026-09-07T10:00:00Z', completed_at: '2026-09-07T10:30:00Z',
      stages: [{ id: 's1', stage_name: 'start', status: 'completed', iteration: 0, error: null, started_at: '2026-09-07T10:00:00Z', completed_at: '2026-09-07T10:30:00Z', input_artifacts: { description: 'x' }, output_artifacts: { verdict: 'success', 'output.md': '# hi' } }] }],
  };

  it('renders flows, stages, gate panel; approve posts and invalidates', async () => {
    vi.mocked(api.tree).mockResolvedValue(tree);
    vi.mocked(api.decisions).mockResolvedValue([]);
    vi.mocked(api.approve).mockResolvedValue({ id: 'd1', run_id: 'r1', gate_instance_id: 'gate_r1', approved: true, status: 'running', state_version: 4, disposition: 'approved', replayed: false });
    withClient(<RunDetail nav={nav} runId="r1" />);
    expect((await screen.findAllByText(/^flow-a/)).length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole('button', { name: /Approval gate/ }));
    expect(await screen.findByText('Approve the plan')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
    await waitFor(() => expect(api.approve).toHaveBeenCalledWith('r1', expect.objectContaining({
      approved: true, gate_instance_id: 'gate_r1', expected_state_version: 3,
      idempotency_key: expect.any(String),
    })));
    expect(await screen.findByText(/approved — run is running \(state 4\)/)).toBeInTheDocument();
    expect(await screen.findByText('2/20 · 18 left')).toBeInTheDocument();
    expect(await screen.findByText(/Active 2m of 2h 0m/)).toBeInTheDocument();
  });

  it('reject opens cause modal and posts structured reason', async () => {
    vi.mocked(api.tree).mockResolvedValue(tree);
    vi.mocked(api.decisions).mockResolvedValue([]);
    vi.mocked(api.approve).mockResolvedValue({ id: 'd2', run_id: 'r1', gate_instance_id: 'gate_r1', approved: false, status: 'running', state_version: 4, disposition: 'rework', replayed: false });
    withClient(<RunDetail nav={nav} runId="r1" />);
    fireEvent.click(await screen.findByRole('button', { name: /Approval gate/ }));
    await screen.findByText('Approve the plan');
    fireEvent.click(screen.getByRole('button', { name: 'Reject with cause' }));
    expect(screen.getByText('Reject with cause', { selector: '.tw-box h2' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /send back for rework/ }));
    await waitFor(() => expect(api.approve).toHaveBeenCalledWith('r1', expect.objectContaining({
      approved: false, reason: 'Scope too broad — needs splitting',
      gate_instance_id: 'gate_r1', expected_state_version: 3,
    })));
  });

  it('shows durable draft-PR publication identity and reconciliation state', async () => {
    vi.mocked(api.tree).mockResolvedValue(tree);
    vi.mocked(api.decisions).mockResolvedValue([]);
    vi.mocked(api.publication).mockResolvedValue({
      run_id: 'r1', draft_pr: 'https://forge/pr/7', reconciliation_required: true, history: [],
      publication: {
        id: 'pub_1', run_id: 'r1', flow_name: 'flow-a', attempt_id: 'a1', operation_key: 'op',
        selection_id: 'sel', task_id: 'T-1', plan_revision_id: 'pln_1', plan_content_hash: 'a'.repeat(64),
        packet_revision_id: 'pkt_1', packet_content_hash: 'b'.repeat(64),
        branch: 'tj/run-task', base_branch: 'main', workspace_path: '/ws', base_sha: 'base', commit_sha: 'c'.repeat(40),
        test_artifact_id: 'art_test', test_artifact_hash: 'c'.repeat(64),
        review_artifact_id: 'art_review', review_artifact_hash: 'd'.repeat(64),
        acceptance_artifact_id: 'art_acc', acceptance_artifact_hash: 'e'.repeat(64),
        scope_files_json: '[]', scope_violations_json: '[]',
        push_state: 'succeeded', push_error: null, remote_sha: 'c'.repeat(40),
        pr_state: 'unknown', pr_number: 7, pr_url: 'https://forge/pr/7', pr_error: 'lost response',
        draft: true, created_at: '2026-10-22T00:00:00Z', updated_at: '2026-10-22T00:00:00Z',
      },
    });
    withClient(<RunDetail nav={nav} runId="r1" />);
    expect(await screen.findByText('T-1')).toBeInTheDocument();
    expect(screen.getByText('Draft PR #7 (humans merge)')).toBeInTheDocument();
    expect(screen.getByText(/reconciles with the remote before any repeated push/)).toBeInTheDocument();
  });
});

describe('RunDetail interrupted recovery', () => {
  it('shows interrupted state, budget exhaustion and starts an explicit retry', async () => {
    const interrupted: TjRun = { ...runs[2], id: 'r9', description: 'Interrupted run', status: 'interrupted' };
    vi.mocked(api.tree).mockResolvedValue({ run: interrupted, flows: [] });
    vi.mocked(api.decisions).mockResolvedValue([]);
    vi.mocked(api.budget).mockResolvedValue({
      run_id: 'r9', question_rounds_used: 6, question_rounds_limit: 6, question_rounds_remaining: 0,
      question_rounds_per_stage_limit: 6, model_calls_used: 20, model_calls_limit: 20, model_calls_remaining: 0,
      tool_calls_used: 0, tool_calls_limit: 20, tool_calls_remaining: 20,
      output_corrections_used: 0, output_corrections_limit: 4, output_corrections_remaining: 4,
      interaction_nudges_used: 0, interaction_nudges_limit: 4, interaction_nudges_remaining: 4,
      active_ms_used: 7200000, active_ms_limit: 7200000, active_ms_remaining: 0,
      cost_micros_used: 0, cost_micros_limit: 0, cost_micros_remaining: 0, unknown_usage_calls: 0,
      exhausted: true, exhausted_reason: 'question_rounds exhausted (6/6)', updated_at: '2026-09-07T10:00:00Z',
    });
    vi.mocked(api.recovery).mockResolvedValue({
      recovery: { run_id: 'r9', interrupted_at: '2026-09-07T10:00:00Z', reason: 'restart',
        resume_flow: 'flow-a', resume_stage: 'start', effects_unknown: false, retry_count: 0, updated_at: '2026-09-07T10:00:00Z' },
      unresolved_effects: [],
    });
    vi.mocked(api.diagnostics).mockResolvedValue({
      run_id: 'r9', status: 'interrupted', state_version: 3, gate_instance_id: null, gate_name: null,
      generated_at: '2026-09-07T10:00:00Z', retention_days: 30, stages: [], routes: [],
      corrections: { llm_calls: 0, output_corrections: 0, silent_turn_nudges: 0 },
      provider_usage: { calls: 0, known_usage_calls: 0, unknown_usage_calls: 0, input_tokens: 0, output_tokens: 0, cost_micros: 0 },
      provider_attempts: { total: 0, succeeded: 0, rate_limited: 0, transient: 0, credential: 0, permanent: 0 },
      budget: null, recovery: null,
    });
    vi.mocked(api.retry).mockResolvedValue({ accepted: true,
      plan: { run_id: 'r9', flow_name: 'flow-a', attempt_offset: 1, effects_unknown_acknowledged: false } });
    withClient(<RunDetail nav={nav} runId="r9" />);
    expect(await screen.findByText(/Run interrupted by a restart/)).toBeInTheDocument();
    expect(await screen.findByText(/Budget exhausted: question_rounds exhausted/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Start new attempt' }));
    await waitFor(() => expect(api.retry).toHaveBeenCalledWith('r9', false));
  });
});

const WF = {
  id: 'wf1', name: 'Synthetic workflow', smg_path: 'fixture.json',
  version: 1, revision: 1, description: 'desc', entry: 'flow-a', exit: 'flow-b',
  flow_count: 2, gate_count: 1, stage_count: 1, created_at: '2026-09-08T00:00:00Z',
  content: {
    entry: 'flow-a', exit: 'flow-b', 'flow-order': ['flow-a', 'gate-a'],
    flows: {
      'flow-a': { type: 'flow' as const, entry: 'start', stages: { start: { agent: 'fixture.json', interactive: true, routes: { success: '_next-flow' } } } },
      'gate-a': { type: 'gate' as const, stages: {} },
    },
  },
  lanes: [],
};

describe('Workflows', () => {
  it('list renders registry rows without the graph', async () => {
    vi.mocked(api.workflows).mockResolvedValue({ workflows: [WF], configured: { sources: [{ id: 'fixture', path: 'fixture.json', kind: 'file' }] } });
    withClient(<Workflows nav={nav} />);
    expect(await screen.findByText('Synthetic workflow')).toBeInTheDocument();
    expect(screen.getByText(/2 flows .* 1 stages/)).toBeInTheDocument();
    expect(screen.queryByText('interactive')).not.toBeInTheDocument();
  });

  it('detail page renders graph and flow order', async () => {
    vi.mocked(api.lanes).mockResolvedValue({ lanes: [] });
    vi.mocked(api.workflow).mockResolvedValue(WF);
    vi.mocked(api.workflows).mockResolvedValue({ workflows: [WF], configured: { sources: [{ id: 'fixture', path: 'fixture.json', kind: 'file' }] } });
    withClient(<WorkflowDetailPage nav={nav} wfId="wf1" />);
    expect((await screen.findAllByText('Synthetic workflow')).length).toBeGreaterThan(0);
    expect(screen.getAllByText('gate-a').length).toBeGreaterThan(0);
    expect(screen.getByText('interactive')).toBeInTheDocument();
  });
});
