import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, newIdempotencyKey } from '../api';
import type { DeliveryBundle, DeliveryCallback, DeliveryDecision, DeliveryRelease } from '../types';

function stateColor(value: string): string {
  if (['succeeded', 'ready', 'passed', 'healthy', 'authorize'].includes(value)) return 'c-done';
  if (['failed', 'blocked', 'cancelled'].includes(value)) return 'c-fail';
  if (['running', 'pending', 'unknown', 'degraded', 'not_requested'].includes(value)) return 'c-wait';
  return 'c-idle';
}

function State({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8, fontSize: 11.5, padding: '1px 0' }}>
      <span className="dim">{label}</span>
      <span className={`chip ${stateColor(value)}`}>{value.replaceAll('_', ' ')}</span>
    </div>
  );
}

function reasons(release: DeliveryRelease): string[] {
  try {
    const parsed = JSON.parse(release.recommendation_reasons_json);
    return Array.isArray(parsed) ? parsed.map(String) : [];
  } catch {
    return [];
  }
}

function Callback({ event }: { event: DeliveryCallback }) {
  return (
    <div className="evt" style={{ alignItems: 'flex-start' }}>
      <time>{event.received_at.slice(11, 16)}</time>
      <span className={`k ${event.accepted ? 'k-ok' : 'k-fail'}`} style={{ marginTop: 4 }} />
      <span style={{ flex: 1, minWidth: 0 }}>
        <span className="mono">{event.event_kind}</span> #{event.sequence} from {event.source}
        {event.accepted
          ? <span className="chip c-done" style={{ marginLeft: 6 }}>accepted</span>
          : <span className="chip c-fail" style={{ marginLeft: 6 }}>rejected</span>}
        {event.rejection_reason && (
          <span className="tiny" style={{ display: 'block', color: 'var(--fail)', wordBreak: 'break-word' }}>
            {event.rejection_reason}
          </span>
        )}
      </span>
    </div>
  );
}

export function DeliveryPanel({ runId }: { runId: string }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ['delivery', runId], queryFn: () => api.delivery(runId), refetchInterval: 5000 });
  const refresh = () => qc.invalidateQueries({ queryKey: ['delivery', runId] });

  const start = useMutation({
    mutationFn: (request: Parameters<typeof api.startDeliveryRelease>[1]) => api.startDeliveryRelease(runId, request),
    onSuccess: refresh,
  });
  const authorize = useMutation({
    mutationFn: (request: Parameters<typeof api.authorizeDelivery>[1]) => api.authorizeDelivery(runId, request),
    onSuccess: refresh,
  });
  const reconcile = useMutation({
    mutationFn: (releaseId: string) => api.reconcileDelivery(runId, releaseId),
    onSuccess: refresh,
  });
  const retry = useMutation({
    mutationFn: (releaseId: string) => api.retryDeliveryDispatch(runId, releaseId),
    onSuccess: refresh,
  });

  if (q.isLoading) return <p className="tiny dim">Loading delivery…</p>;
  if (q.error || !q.data) return <p className="tiny dim">Unable to load delivery evidence: {(q.error as Error | null)?.message || 'unknown error'}</p>;
  const data: DeliveryBundle = q.data;
  const releases = data.releases;
  const decisions = data.decisions;
  const staging = releases.filter((release) => release.environment === 'staging' && release.kind === 'deploy');
  const productionReleases = releases.filter((release) => release.environment === 'production');
  const latestStaging = staging[staging.length - 1];
  const productionAuthorized = decisions.filter((decision) => decision.decision === 'authorize' && decision.kind === 'deploy');
  const releaseForDecision = (decision: DeliveryDecision) => productionReleases.find((release) => release.authorization_id === decision.id);
  const isLatestDecision = (decision: DeliveryDecision) => decisions
    .filter((candidate) => candidate.publication_id === decision.publication_id
      && candidate.kind === decision.kind
      && candidate.expected_revision === decision.expected_revision)
    .at(-1)?.id === decision.id;

  const startStaging = () => start.mutate({ environment: 'staging' });
  const authorizeProduction = (release: DeliveryRelease) => {
    const reason = window.prompt('Reason for authorizing production deployment of the exact verified commit?');
    if (!reason || !reason.trim()) return;
    authorize.mutate({
      publication_id: release.publication_id,
      kind: 'deploy',
      staging_release_id: release.id,
      reason: reason.trim(),
      idempotency_key: newIdempotencyKey(),
    });
  };
  const authorizeRollback = (release: DeliveryRelease) => {
    const target = window.prompt('Exact commit SHA to roll back to (must be a previously known-good revision).');
    if (!target || !target.trim()) return;
    const reason = window.prompt('Reason for authorizing this rollback of the exact target revision?');
    if (!reason || !reason.trim()) return;
    authorize.mutate({
      publication_id: release.publication_id,
      kind: 'rollback',
      source_release_id: release.id,
      rollback_target_sha: target.trim(),
      reason: reason.trim(),
      idempotency_key: newIdempotencyKey(),
    });
  };

  const actionError = (start.error || authorize.error || reconcile.error || retry.error) as Error | null;

  return (
    <div>
      <p className="label" style={{ margin: 0 }}>External delivery <span className="chip c-idle">external CI/CD</span></p>
      <p className="tiny dim" style={{ margin: '3px 0 6px' }}>
        TurboJet dispatches a Forgejo Actions-compatible workflow and records signed evidence. It never deploys, merges or releases by itself.
      </p>
      {!data.integration.configured && (
        <p className="tiny" style={{ color: 'var(--wait)', margin: '0 0 6px' }}>
          {data.integration.limitation}
        </p>
      )}
      {data.integration.configured && (
        <p className="tiny dim" style={{ margin: '0 0 6px' }}>
          {data.integration.provider} · {data.integration.repository} · {data.integration.workflow}
          <br />callback window {data.integration.callback_window_secs}s
        </p>
      )}
      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginBottom: 6 }}>
        <button className="btn btn-sm" disabled={start.isPending || !data.integration.configured} onClick={startStaging}
          title={latestStaging ? 'Replay returns the exact existing operation without a second dispatch' : 'Start staging first'}>
          Start staging release
        </button>
      </div>
      {actionError && <p className="tiny" style={{ color: 'var(--fail)', margin: '0 0 6px', wordBreak: 'break-word' }}>{actionError.message}</p>}
      {!releases.length && <p className="tiny dim">No external delivery operation has been requested. Engineering completion alone is not a deployment.</p>}
      {releases.map((release) => {
        const auth = decisions.find((decision) => decision.id === release.authorization_id);
        return (
          <div key={release.id} className="card pad" style={{ marginBottom: 8, padding: 9 }}>
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
              <span className={`chip ${release.environment === 'production' ? 'c-fail' : 'c-wait'}`}>{release.environment}</span>
              <strong style={{ fontSize: 12 }}>{release.kind}</strong>
              <span className={`chip ${stateColor(release.recommendation)}`}>{release.recommendation}</span>
              <span className="tiny dim" style={{ marginLeft: 'auto' }}>{release.created_at.slice(0, 16).replace('T', ' ')}</span>
            </div>
            <div style={{ marginTop: 6 }}>
              <State label="Dispatch" value={release.dispatch_state} />
              <State label="CI" value={release.ci_state} />
              <State label="Deployment" value={release.deployment_state} />
              <State label="Independent verification" value={release.verification_state} />
              <State label="Observation" value={release.observation_state} />
            </div>
            <p className="tiny dim" style={{ margin: '4px 0 0', wordBreak: 'break-all' }}>
              task <span className="mono">{release.task_id}</span><br />
              commit <span className="mono">{release.commit_sha.slice(0, 16)}</span>
              {release.kind === 'rollback' && <> → target <span className="mono">{release.expected_revision.slice(0, 16)}</span></>}<br />
              plan <span className="mono">{release.plan_content_hash.slice(0, 12)}</span>
              {' · '}packet <span className="mono">{release.packet_content_hash.slice(0, 12)}</span>
              {' · '}PR #{release.pr_number ?? '—'}
            </p>
            {reasons(release).length > 0 && (
              <ul className="tiny" style={{ margin: '4px 0 0', paddingLeft: 16, color: release.recommendation === 'blocked' ? 'var(--fail)' : 'var(--muted)' }}>
                {reasons(release).map((reason, index) => <li key={index}>{reason}</li>)}
              </ul>
            )}
            {release.dispatch_error && <p className="tiny" style={{ color: 'var(--fail)', margin: '4px 0 0' }}>{release.dispatch_error}</p>}
            {release.last_error && <p className="tiny" style={{ color: 'var(--fail)', margin: '4px 0 0' }}>Rejected/unknown: {release.last_error}</p>}
            <p className="tiny dim" style={{ margin: '4px 0 0', wordBreak: 'break-all' }}>
              operation <span className="mono">{release.operation_key}</span>
              {release.external_url && <><br /><a className="tiny" href={release.external_url} target="_blank" rel="noreferrer">External CI run</a></>}
            </p>
            <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginTop: 6 }}>
              {release.kind === 'deploy' && (
                <button className="btn btn-sm" disabled={reconcile.isPending || !data.integration.configured}
                  onClick={() => reconcile.mutate(release.id)}>Reconcile from CI</button>
              )}
              {['failed', 'unknown'].includes(release.dispatch_state) && !release.external_run_id && (
                <button className="btn btn-sm btn-no" disabled={retry.isPending || !data.integration.configured}
                  title="Explicitly reuse the immutable operation key; confirm that an unknown attempt may already have reached the external system"
                  onClick={() => {
                    if (window.confirm('Re-dispatch this exact operation? An earlier unknown attempt may already have reached external CI. The workflow must deduplicate by operation key.')) {
                      retry.mutate(release.id);
                    }
                  }}>Retry dispatch</button>
              )}
              {release.environment === 'staging' && release.recommendation === 'ready' && !productionAuthorized.some((decision) => decision.staging_release_id === release.id) && (
                <button className="btn btn-sm btn-go" disabled={authorize.isPending} onClick={() => authorizeProduction(release)}>
                  Authorize production revision
                </button>
              )}
              {release.environment === 'production' && release.kind === 'deploy' && ['succeeded', 'failed'].includes(release.deployment_state) && (
                <button className="btn btn-sm btn-no" disabled={authorize.isPending} onClick={() => authorizeRollback(release)}>
                  Authorize rollback
                </button>
              )}
            </div>
            {auth && <p className="tiny dim" style={{ margin: '4px 0 0' }}>authorized by {auth.actor_name}: {auth.reason || '—'}</p>}
          </div>
        );
      })}

      {decisions.length > 0 && (
        <>
          <p className="label" style={{ marginTop: 10 }}>Immutable decisions</p>
          {decisions.map((decision) => {
            const existing = releaseForDecision(decision);
            const used = Boolean(existing) || decision.decision === 'reject';
            return (
              <div key={decision.id} className="evt" style={{ alignItems: 'flex-start' }}>
                <time>{decision.created_at.slice(11, 16)}</time>
                <span className={`k ${decision.decision === 'authorize' ? 'k-ok' : 'k-fail'}`} style={{ marginTop: 4 }} />
                <span style={{ flex: 1, minWidth: 0 }}>
                  <span className={`chip ${stateColor(decision.decision)}`}>{decision.decision}</span>{' '}
                  {decision.kind} {decision.environment} · {decision.actor_name}
                  {!isLatestDecision(decision) && <span className="chip c-idle" style={{ marginLeft: 6 }}>superseded</span>}
                  <span className="tiny dim" style={{ display: 'block', wordBreak: 'break-all' }}>
                    exact revision <span className="mono">{decision.expected_revision.slice(0, 20)}</span>
                    {decision.staging_release_id && <><br />staging <span className="mono">{decision.staging_release_id}</span></>}
                    {decision.source_release_id && <><br />source <span className="mono">{decision.source_release_id}</span></>}
                  </span>
                  {decision.reason && <span className="tiny dim" style={{ display: 'block' }}>{decision.reason}</span>}
                  {decision.decision === 'authorize' && !used && isLatestDecision(decision) && (
                    <button className="btn btn-sm btn-go" style={{ marginTop: 4 }} disabled={start.isPending || !data.integration.configured}
                      onClick={() => start.mutate({
                        environment: 'production', kind: decision.kind,
                        publication_id: decision.publication_id,
                        authorization_id: decision.id,
                        source_release_id: decision.source_release_id || undefined,
                        rollback_target_sha: decision.kind === 'rollback' ? decision.expected_revision : undefined,
                      })}>
                      Start {decision.kind} for exact revision
                    </button>
                  )}
                </span>
              </div>
            );
          })}
        </>
      )}

      {data.callbacks.length > 0 && (
        <>
          <p className="label" style={{ marginTop: 10 }}>Signed callback audit</p>
          {data.callbacks.slice(-12).map((event) => <Callback key={event.id} event={event} />)}
        </>
      )}
      <p className="tiny dim" style={{ marginTop: 6 }}>
        Pending/unknown states stay visible. A rejected or out-of-order callback is retained as an audit row and never changes the release state.
      </p>
    </div>
  );
}
