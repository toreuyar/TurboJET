import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { PlanReview } from '../../src/views/Plan';
import { api } from '../../src/api';
import type { PlanBundle } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    plan: vi.fn(),
    savePlanRevision: vi.fn(),
    downloadPlan: vi.fn(),
    selectPlanTask: vi.fn(),
    planTasks: vi.fn(),
  },
}));

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

const planContent = {
  schema_version: 1,
  title: 'Cancellation task plan',
  summary: 'Traceable tasks for the approved packet.',
  source_packet: { packet_id: 'pkt_1', revision: 2, content_hash: 'a'.repeat(64) },
  tasks: [
    {
      id: 'T-1', title: 'Eligibility and preview', objective: 'Expose eligibility and refund preview.',
      requirement_refs: ['REQ-1', 'REQ-2'], decision_refs: [], dependencies: [],
      acceptance_criteria: ['Eligible bookings show the exact refund preview.'],
      components: ['booking-domain'],
      estimate: { size: 'm', uncertainty: 'medium' },
      risks: [{ id: 'R-1', statement: 'Policy differs by source.', severity: 'major', mitigation: 'Use policy resolver fixtures.' }],
      verification: ['Contract tests cover eligible and ineligible bookings.'],
    },
    {
      id: 'T-2', title: 'Send confirmation', objective: 'Notify after commit.',
      requirement_refs: ['REQ-3'], decision_refs: ['DEC-2'], dependencies: ['T-1'],
      acceptance_criteria: ['Exactly one confirmation is sent.'],
      components: ['notification-worker'],
      estimate: { size: 's', uncertainty: 'high' },
      risks: [], verification: ['Worker test asserts one message.'],
    },
  ],
  scope_expansion_requests: [],
};

const latest = {
  id: 'plr_2', plan_id: 'pln_1', run_id: 'r1', revision: 2, schema_version: 1,
  content_hash: 'b'.repeat(64), packet_revision_id: 'pr_2', packet_content_hash: 'a'.repeat(64),
  packet_id: 'pkt_1', packet_revision: 2, parent_revision_id: 'plr_1',
  source_artifact_id: 'art_plan', source_artifact_hash: 'c'.repeat(64),
  flow_name: 'planning', stage_name: 'plan', change_request: 'split notification task',
  created_by: 'user:admin', created_at: '2026-10-22T00:00:00Z',
  content: planContent,
  validation: {
    valid: true, task_count: 2, requires_packet_revision: false,
    blocked_tasks: [{ task_id: 'T-2', reasons: ['blocked by open decision DEC-2: Which notification channel?'] }],
    open_decisions: ['DEC-2'],
  },
};

const previous = {
  ...latest, id: 'plr_1', revision: 1, content_hash: 'd'.repeat(64), parent_revision_id: null,
  content: { ...planContent, title: 'Older cancellation task plan' },
  validation: { ...latest.validation, blocked_tasks: [] },
};

function bundle(overrides: Partial<PlanBundle> = {}): PlanBundle {
  return {
    run_id: 'r1', plan_id: 'pln_1', latest, approved: null, revisions: [previous, latest],
    reviews: [], gate_binding: null,
    readiness: [
      { task_id: 'T-1', title: 'Eligibility and preview', ready: true, dependencies: [], requirement_refs: ['REQ-1'], acceptance_criteria: ['Eligible bookings show the exact refund preview.'], components: ['booking-domain'], verification: ['Contract tests cover eligible and ineligible bookings.'], blocked_reasons: [] },
      { task_id: 'T-2', title: 'Send confirmation', ready: false, dependencies: ['T-1'], requirement_refs: ['REQ-3'], acceptance_criteria: ['Exactly one confirmation is sent.'], components: ['notification-worker'], verification: ['Worker test asserts one message.'], blocked_reasons: ['blocked by open decision DEC-2: Which notification channel?'] },
    ],
    selections: [],
    ...overrides,
  };
}

describe('PlanReview', () => {
  beforeEach(() => vi.clearAllMocks());

  it('shows tasks, dependencies, evidence, blocked reason, validation and experimental label', async () => {
    vi.mocked(api.plan).mockResolvedValue(bundle());
    withClient(<PlanReview runId="r1" />);
    expect(await screen.findByText('Structured task plan')).toBeInTheDocument();
    expect(screen.getAllByText('experimental').length).toBeGreaterThan(0);
    expect(screen.getByText(/T-1 — Eligibility and preview/)).toBeInTheDocument();
    expect(screen.getByText(/Requirements: REQ-1, REQ-2/)).toBeInTheDocument();
    expect(screen.getByText(/Dependencies: T-1 · Components: notification-worker/)).toBeInTheDocument();
    expect(screen.getByText(/blocked by open decision DEC-2/)).toBeInTheDocument();
    expect(screen.getByText(/Deterministic validation precedes the human gate/)).toBeInTheDocument();
    expect(screen.getByText(/Planning is read-only/)).toBeInTheDocument();
    expect(screen.getByText(/Changes since previous plan revision/)).toBeInTheDocument();
    expect(screen.getByText(/Older cancellation task plan/)).toBeInTheDocument();
    for (const button of screen.getAllByRole('button', { name: 'Select task' })) expect(button).toBeDisabled();
  });

  it('downloads the exact selected revision and saves a reviewer revision without inherited approval', async () => {
    vi.mocked(api.plan).mockResolvedValue(bundle());
    vi.mocked(api.downloadPlan).mockResolvedValue({ filename: 'pln_1-r2.md' });
    vi.mocked(api.savePlanRevision).mockResolvedValue({ ...latest, id: 'plr_3', revision: 3 });
    withClient(<PlanReview runId="r1" />);
    await screen.findByText('Structured task plan');
    fireEvent.click(screen.getByRole('button', { name: 'Download Markdown' }));
    await waitFor(() => expect(api.downloadPlan).toHaveBeenCalledWith('r1', 'markdown', 'plr_2'));

    fireEvent.click(screen.getByRole('button', { name: 'Revise plan' }));
    const editor = screen.getByLabelText('Plan JSON (task-plan schema v1)');
    fireEvent.change(editor, { target: { value: JSON.stringify({ ...planContent, title: 'Revised task plan' }, null, 2) } });
    fireEvent.change(screen.getByLabelText('Reason for the revision'), { target: { value: 'clarify criteria' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save plan revision' }));
    await waitFor(() => expect(api.savePlanRevision).toHaveBeenCalledWith('r1', expect.objectContaining({ title: 'Revised task plan' }), {
      planId: 'pln_1', changeRequest: 'clarify criteria',
    }));
    expect(await screen.findByText(/any earlier plan approval is stale/)).toBeInTheDocument();
  });

  it('selects only a ready task from the exact approved plan revision and retains criteria/provenance', async () => {
    vi.mocked(api.plan).mockResolvedValue(bundle({ approved: latest }));
    vi.mocked(api.selectPlanTask).mockResolvedValue({
      selection: { id: 'pts_1', task_id: 'T-1', plan_revision_id: 'plr_2', plan_content_hash: 'b'.repeat(64), selected_by: 'user:admin', created_at: '2026-10-22T01:00:00Z' },
      payload: { task: planContent.tasks[0], plan: { content_hash: 'b'.repeat(64) } },
      replayed: false,
    });
    withClient(<PlanReview runId="r1" />);
    await screen.findByText('Structured task plan');
    const buttons = screen.getAllByRole('button', { name: 'Select task' });
    expect(buttons[0]).toBeEnabled();
    expect(buttons[1]).toBeDisabled();
    fireEvent.click(buttons[0]);
    await waitFor(() => expect(api.selectPlanTask).toHaveBeenCalledWith('r1', {
      plan_revision_id: 'plr_2', plan_content_hash: 'b'.repeat(64), task_id: 'T-1',
    }));
    expect(await screen.findByText(/criteria and provenance are retained in the handoff payload/)).toBeInTheDocument();
  });
});
