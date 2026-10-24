import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { DeliveryPanel } from '../../src/views/Delivery';
import { api } from '../../src/api';
import type { DeliveryBundle, DeliveryRelease } from '../../src/types';

vi.mock('../../src/api', () => ({
  api: {
    delivery: vi.fn(),
    startDeliveryRelease: vi.fn(),
    authorizeDelivery: vi.fn(),
    reconcileDelivery: vi.fn(),
    retryDeliveryDispatch: vi.fn(),
  },
  newIdempotencyKey: vi.fn(() => 'test-key'),
}));

const release: DeliveryRelease = {
  id: 'drel_1', run_id: 'r1', project_id: 'p1', publication_id: 'pub1',
  selection_id: 'sel1', task_id: 'T-1',
  plan_revision_id: 'planrev', plan_content_hash: 'a'.repeat(64),
  packet_revision_id: 'pktrev', packet_content_hash: 'b'.repeat(64),
  pr_number: 12, pr_url: 'https://forge/pr/12', commit_sha: 'c'.repeat(40),
  base_sha: 'base', branch: 'tj/task', environment: 'staging', kind: 'deploy',
  expected_revision: 'c'.repeat(40), authorization_id: null, source_release_id: null,
  operation_key: 'deliveryop:123', workflow: 'delivery.yml', workflow_ref: 'main',
  dispatch_state: 'failed', external_run_id: null, external_url: null,
  dispatch_error: null, ci_state: 'succeeded', ci_run_id: '42', ci_url: null,
  deployment_state: 'succeeded', deployed_revision: 'c'.repeat(40),
  deployment_run_id: 'deploy-1', deployment_url: null, deployment_detail: null,
  verification_state: 'failed', verified_revision: null,
  verifier: 'independent-probe', verification_evidence_hash: 'd'.repeat(64),
  verification_detail: 'health check failed', observation_state: 'pending',
  observation_evidence_hash: null, observation_detail: null,
  recommendation: 'blocked',
  recommendation_reasons_json: JSON.stringify(['independent verification failed', 'observation is pending']),
  last_sequence: 4, last_error: 'stale/out-of-order callback sequence 2; last applied sequence is 4',
  created_by: 'u1', created_at: '2026-10-22T10:00:00Z', updated_at: '2026-10-22T10:05:00Z',
};

const bundle: DeliveryBundle = {
  run_id: 'r1',
  releases: [release],
  decisions: [],
  callbacks: [{
    id: 1, release_id: release.id, event_id: 'stale-2', source: 'forgejo-actions',
    event_kind: 'ci', sequence: 2, payload_json: '{}', payload_hash: 'e'.repeat(64),
    accepted: false, rejection_reason: 'stale/out-of-order callback sequence 2; last applied sequence is 4',
    received_at: '2026-10-22T10:06:00Z',
  }],
  integration: {
    configured: true, provider: 'forgejo_actions', repository: 'owner/repo',
    workflow: 'delivery.yml', git_ref: 'main', callback_window_secs: 300,
    callback_path_template: '/v1/delivery/releases/{release_id}/callbacks',
    execution_authority: 'external_ci_cd', limitation: 'local contract only',
  },
  execution_authority: 'external_ci_cd',
  note: 'external CI executes',
};

function withClient(el: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);
}

describe('DeliveryPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.delivery).mockResolvedValue(bundle);
  });

  it('shows exact identities, blocked recommendation reasons and rejected callbacks', async () => {
    withClient(<DeliveryPanel runId="r1" />);
    expect(await screen.findByText('External delivery')).toBeInTheDocument();
    expect(screen.getByText('forgejo_actions · owner/repo · delivery.yml', { exact: false })).toBeInTheDocument();
    expect(screen.getByText('independent verification failed')).toBeInTheDocument();
    expect(screen.getByText('stale/out-of-order callback sequence 2; last applied sequence is 4')).toBeInTheDocument();
    expect(screen.getByText('Retry dispatch')).toBeInTheDocument();
  });
});
