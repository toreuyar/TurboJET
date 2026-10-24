import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RunDetail } from '../../src/views/Runs';
import { api } from '../../src/api';
import type { Nav } from '../../src/App';
import type { RunTree, TjRun } from '../../src/types';

/**
 * Optional SDLC-domain UI contract. The default unit suite uses neutral
 * synthetic flows; this shipped behavior name is exercised only when the
 * example suite is selected (`npm run test:examples`).
 */
vi.mock('../../src/api', () => ({
  api: {
    runs: vi.fn(), tree: vi.fn(), decisions: vi.fn(), approve: vi.fn(), cancel: vi.fn(),
    budget: vi.fn(), diagnostics: vi.fn(), publication: vi.fn(), delivery: vi.fn(),
    readiness: vi.fn(), projects: vi.fn(), downloadSupportExport: vi.fn(), packet: vi.fn(),
    plan: vi.fn(), savePlanRevision: vi.fn(), downloadPlan: vi.fn(), selectPlanTask: vi.fn(),
    planTasks: vi.fn(), savePacketRevision: vi.fn(), downloadPacket: vi.fn(),
    recovery: vi.fn(), retry: vi.fn(), createRun: vi.fn(), workflows: vi.fn(),
    workflow: vi.fn(), lanes: vi.fn(), chat: vi.fn(),
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

const run: TjRun = {
  id: 'r1', project_id: null, lane_id: null, description: 'Domain flow', status: 'waiting_for_gate',
  current_flow_id: null, gate_state: 'task_selection_required', gate_instance_id: null,
  gate_name: null, state_version: 3, repo_url: null, base_branch: 'main', slug: null,
  pr_url: null, pr_number: null, created_at: '2026-09-07T10:00:00Z', updated_at: '2026-09-07T11:00:00Z',
};

const tree: RunTree = {
  run,
  flows: [{ id: 'f1', flow_name: 'engineering', flow_number: 1, status: 'completed', branch_name: 'tj/x', started_at: '2026-09-07T10:00:00Z', completed_at: '2026-09-07T10:30:00Z', stages: [] }],
};

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

describe('shipped SDLC task-selection gate UI', () => {
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
      run_id: 'r1', status: 'waiting_for_gate', state_version: 3, gate_instance_id: null, gate_name: null,
      generated_at: '2026-09-07T10:00:00Z', retention_days: 30, stages: [], routes: [],
      corrections: { llm_calls: 2, output_corrections: 0, silent_turn_nudges: 0 },
      provider_usage: { calls: 2, known_usage_calls: 2, unknown_usage_calls: 0, input_tokens: 10, output_tokens: 20, cost_micros: 0 },
      provider_attempts: { total: 2, succeeded: 2, rate_limited: 0, transient: 0, credential: 0, permanent: 0 },
      budget: null, recovery: null,
    });
    vi.mocked(api.packet).mockResolvedValue({ run_id: 'r1', packet_id: null, latest: null, revisions: [], reviews: [], gate_binding: null, allowed_source_references: [] });
    vi.mocked(api.plan).mockResolvedValue({ run_id: 'r1', plan_id: null, latest: null, approved: null, revisions: [], reviews: [], gate_binding: null, readiness: [], selections: [] });
    vi.mocked(api.tree).mockResolvedValue(tree);
    vi.mocked(api.decisions).mockResolvedValue([]);
  });

  it('waits for an explicit ready-task selection before engineering', async () => {
    withClient(<RunDetail nav={nav} runId="r1" />);
    expect(await screen.findByText(/Plan approved\. The selected flow is/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Select a ready task/ })).toBeInTheDocument();
    expect(api.approve).not.toHaveBeenCalled();
  });
});
