import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { PacketReview } from '../../src/views/Packet';
import { RunDetail } from '../../src/views/Runs';
import { api } from '../../src/api';
import type { Nav } from '../../src/App';
import type { PacketBundle, PacketRevision, RunTree, TjRun } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    packet: vi.fn(),
    plan: vi.fn(),
    savePacketRevision: vi.fn(),
    downloadPacket: vi.fn(),
    tree: vi.fn(),
    decisions: vi.fn(),
    approve: vi.fn(),
    cancel: vi.fn(),
    budget: vi.fn(),
    recovery: vi.fn(),
    retry: vi.fn(),
  },
  subscribeEvents: vi.fn(() => () => {}),
  getToken: vi.fn(() => 'tok'),
  setToken: vi.fn(),
}));

const nav: Nav = {
  view: 'run', projectId: null, runId: 'run_packet', wfId: null,
  setView: vi.fn(), openRun: vi.fn(), openWorkflow: vi.fn(), setProject: vi.fn(),
};

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

const packetContent = {
  schema_version: 1,
  title: 'Booking cancellation',
  problem: { statement: 'Solo travellers cannot cancel online.', source_refs: ['SRC-1'] },
  requirements: [{
    id: 'REQ-1', title: 'Online cancellation', statement: 'Offer a cancel action.', kind: 'functional',
    priority: 'must', status: 'confirmed', acceptance_criteria: ['Eligible booking can cancel once.'], source_refs: ['SRC-1'],
  }],
  evidence: [{ id: 'EVD-1', claim: 'Support handles cancels manually.', source_refs: ['SRC-1'] }],
  unknowns: [{ id: 'UNK-1', question: 'Which refund window?', impact: 'Acceptance is incomplete.', owner: 'product-owner', source_refs: ['SRC-1'] }],
  decisions: [{ id: 'DEC-1', question: 'Show exact date or range?', owner: 'product-owner', status: 'open', decision: null, source_refs: ['SRC-1'] }],
  contradictions: [],
  risks: [],
  sources: [{ id: 'SRC-1', kind: 'stage_artifact', reference: 'artifact:discovery:intake', excerpt: 'Cancel online.' }],
};

const revision1: PacketRevision = {
  id: 'pr_1', packet_id: 'pkt_1', run_id: 'run_packet', revision: 1, schema_version: 1,
  content_hash: 'a'.repeat(64), completeness: 'complete', parent_revision_id: null,
  source_artifact_id: 'art_1', source_artifact_hash: 'b'.repeat(64), flow_name: 'discovery', stage_name: 'packet',
  change_request: null, created_by: 'system:packet-generation', created_at: '2026-10-22T10:00:00Z',
  content: { ...packetContent, unknowns: [], decisions: [] },
};
const revision2: PacketRevision = {
  ...revision1, id: 'pr_2', revision: 2, content_hash: 'c'.repeat(64), completeness: 'incomplete',
  parent_revision_id: 'pr_1', change_request: 'clarify refund timing', content: packetContent,
};
const bundle: PacketBundle = {
  run_id: 'run_packet', packet_id: 'pkt_1', latest: revision2, revisions: [revision1, revision2],
  reviews: [{
    id: 'rev_1', revision_id: 'pr_1', revision: 1, content_hash: revision1.content_hash,
    gate_instance_id: 'gate_old', actor_id: 'u1', actor_name: 'Reviewer',
    outcome: 'changes_requested', reason: 'clarify refund timing', created_at: '2026-10-22T10:05:00Z',
  }],
  gate_binding: {
    gate_instance_id: 'gate_1', packet_id: 'pkt_1', revision_id: 'pr_2', revision: 2,
    content_hash: revision2.content_hash, completeness: 'incomplete',
  },
  allowed_source_references: ['artifact:discovery:intake'],
};

const run: TjRun = {
  id: 'run_packet', project_id: 'proj_1', lane_id: 'lane_1', description: 'Packet run', status: 'waiting_for_gate',
  current_flow_id: 'f1', gate_state: 'Review the requirements packet', gate_instance_id: 'gate_1',
  gate_name: 'discovery-gate', state_version: 7, repo_url: null, base_branch: 'main', slug: null,
  pr_url: null, pr_number: null, created_at: '2026-10-22T10:00:00Z', updated_at: '2026-10-22T10:10:00Z',
  gate_binding: {
    kind: 'packet', gate_instance_id: 'gate_1', flow_name: null, stage_name: null,
    artifact_id: null, artifact_version: null, content_hash: revision2.content_hash,
    packet_revision_id: 'pr_2', packet_content_hash: revision2.content_hash,
    plan_revision_id: null, plan_content_hash: null,
  },
};
const tree: RunTree = {
  run,
  flows: [{
    id: 'f1', flow_name: 'discovery', flow_number: 1, status: 'completed', branch_name: null,
    started_at: '2026-10-22T09:50:00Z', completed_at: '2026-10-22T10:00:00Z',
    stages: [{
      id: 's1', stage_name: 'packet', status: 'completed', iteration: 0, error: null,
      started_at: '2026-10-22T09:50:00Z', completed_at: '2026-10-22T10:00:00Z',
      input_artifacts: null, output_artifacts: { verdict: 'generated', packet: packetContent },
    }],
  }],
};

describe('PacketReview', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.packet).mockResolvedValue(bundle);
    vi.mocked(api.plan).mockResolvedValue({
      run_id: 'run_packet', plan_id: null, latest: null, approved: null, revisions: [], reviews: [],
      gate_binding: null, readiness: [], selections: [],
    });
  });

  it('shows contents, evidence, unknowns, diff, history and exports the same revision', async () => {
    vi.mocked(api.downloadPacket).mockResolvedValue({ filename: 'pkt_1-r2.md' });
    withClient(<PacketReview runId="run_packet" />);
    expect(await screen.findByText('Booking cancellation')).toBeInTheDocument();
    expect(screen.getByText(/Solo travellers cannot cancel online/)).toBeInTheDocument();
    expect(screen.getByText(/REQ-1/)).toBeInTheDocument();
    fireEvent.click(screen.getByText(/Evidence \(1\)/));
    expect(await screen.findByText(/Support handles cancels manually/)).toBeInTheDocument();
    expect(screen.getAllByText(/Which refund window/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/Show exact date or range/).length).toBeGreaterThan(0);
    fireEvent.click(screen.getByText(/Changes since previous revision/));
    expect(await screen.findByLabelText('Diff from revision 1')).toBeInTheDocument();
    expect(screen.getByText(/changes_requested/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Download JSON' }));
    await waitFor(() => expect(api.downloadPacket).toHaveBeenCalledWith('run_packet', 'json', 'pr_2'));
  });

  it('saves a reviewer revision without inheriting approval', async () => {
    vi.mocked(api.savePacketRevision).mockResolvedValue({ ...revision2, id: 'pr_3', revision: 3, replayed: false });
    withClient(<PacketReview runId="run_packet" />);
    await screen.findByText('Booking cancellation');
    fireEvent.click(screen.getByRole('button', { name: 'Revise packet' }));
    const editor = screen.getByLabelText('Packet JSON (schema v1)');
    fireEvent.change(editor, { target: { value: JSON.stringify({ ...packetContent, title: 'Booking cancellation v2' }, null, 2) } });
    fireEvent.change(screen.getByLabelText('Reason for the revision'), { target: { value: 'clarify refund timing' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save revision' }));
    await waitFor(() => expect(api.savePacketRevision).toHaveBeenCalledWith('run_packet', expect.objectContaining({ title: 'Booking cancellation v2' }), {
      packetId: 'pkt_1', changeRequest: 'clarify refund timing',
    }));
    expect(await screen.findByText(/Revision 3 saved/)).toBeInTheDocument();
  });
});

describe('Discovery gate packet approval', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.packet).mockResolvedValue(bundle);
    vi.mocked(api.plan).mockResolvedValue({
      run_id: 'run_packet', plan_id: null, latest: null, approved: null, revisions: [], reviews: [],
      gate_binding: null, readiness: [], selections: [],
    });
    vi.mocked(api.tree).mockResolvedValue(tree);
    vi.mocked(api.decisions).mockResolvedValue([]);
    vi.mocked(api.budget).mockResolvedValue({
      run_id: run.id, question_rounds_used: 1, question_rounds_limit: 6, question_rounds_remaining: 5,
      question_rounds_per_stage_limit: 6, model_calls_used: 2, model_calls_limit: 20, model_calls_remaining: 18,
      tool_calls_used: 0, tool_calls_limit: 20, tool_calls_remaining: 20,
      output_corrections_used: 0, output_corrections_limit: 4, output_corrections_remaining: 4,
      interaction_nudges_used: 0, interaction_nudges_limit: 4, interaction_nudges_remaining: 4,
      active_ms_used: 120000, active_ms_limit: 7200000, active_ms_remaining: 7080000,
      cost_micros_used: 0, cost_micros_limit: 0, cost_micros_remaining: 0, unknown_usage_calls: 0,
      exhausted: false, exhausted_reason: null, updated_at: '2026-10-22T10:00:00Z',
    });
    vi.mocked(api.recovery).mockResolvedValue({ recovery: null, unresolved_effects: [] });
  });

  it('requires acknowledgement for an incomplete packet and posts the exact revision/hash', async () => {
    vi.mocked(api.approve).mockResolvedValue({
      id: 'gd_1', run_id: run.id, gate_instance_id: 'gate_1', approved: true,
      status: 'completed', state_version: 8, disposition: 'approved', replayed: false,
    });
    withClient(<RunDetail nav={nav} runId={run.id} />);
    fireEvent.click(await screen.findByRole('button', { name: /Approval gate/ }));
    const approve = await screen.findByRole('button', { name: 'Approve' });
    expect(approve).toBeDisabled();
    fireEvent.click(await screen.findByRole('checkbox'));
    expect(approve).toBeEnabled();
    fireEvent.click(approve);
    await waitFor(() => expect(api.approve).toHaveBeenCalledWith(run.id, expect.objectContaining({
      approved: true, gate_instance_id: 'gate_1', expected_state_version: 7,
      packet_revision_id: 'pr_2', packet_content_hash: revision2.content_hash,
      acknowledge_incomplete: true,
    })));
  });

  it('blocks approval when the bound revision is contradictory', async () => {
    const contradictory: PacketBundle = {
      ...bundle,
      latest: { ...revision2, completeness: 'contradictory' },
      revisions: [revision1, { ...revision2, completeness: 'contradictory' }],
      gate_binding: { ...bundle.gate_binding!, completeness: 'contradictory' },
    };
    vi.mocked(api.packet).mockResolvedValue(contradictory);
    withClient(<RunDetail nav={nav} runId={run.id} />);
    fireEvent.click(await screen.findByRole('button', { name: /Approval gate/ }));
    expect(await screen.findByText(/contains unresolved contradictions/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled();
    expect(api.approve).not.toHaveBeenCalled();
  });
});
