import { useEffect, useMemo, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import { MarkdownView, ArtifactViewerPane, FilePreview, ViewerContext, type ViewerArtifact } from '../chat/render';
import { Virtuoso } from 'react-virtuoso';
import type { GateDecision, QuestionRequest, RunBudget, StageLog, StageQuestion, TjArtifact, TjFlow, TjRun, TjStage } from '../types';
import { REJECT_REASONS } from '../types';
import { Dot, StatusChip, useRunTree, type Nav } from '../App';
import { PacketReview } from './Packet';
import { PlanReview } from './Plan';
import { DeliveryPanel } from './Delivery';

type Filter = 'all' | 'active' | 'done' | 'failed';

const TERMINAL = ['completed', 'failed', 'cancelled', 'interrupted'];

/// Stop an in-flight run (running / waiting at a gate / waiting for input).
function CancelRunButton({ run }: { run: TjRun }) {
  const qc = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const cancel = useMutation({
    mutationFn: () => api.cancel(run.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['tree', run.id] });
      qc.invalidateQueries({ queryKey: ['events', run.id] });
    },
  });
  if (TERMINAL.includes(run.status)) return null;
  return (
    <>
      <button className="btn btn-sm btn-no" title="Cancel this run" onClick={(e) => { e.stopPropagation(); setConfirm(true); }}>Cancel</button>
      {confirm && (
        <Modal title="Cancel this run?" onClose={() => setConfirm(false)}>
          <p className="tiny dim" style={{ margin: '0 0 12px' }}>
            Stops <strong>{run.description || run.id}</strong> where it is. The running stage is told to stop, the
            workspace/worktree is cleaned up, and the run is marked <em>cancelled</em> — it can be deleted afterwards.
          </p>
          {cancel.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>{(cancel.error as Error).message}</p>}
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-no" disabled={cancel.isPending} onClick={() => cancel.mutate()}>Cancel the run</button>
            <button className="btn" onClick={() => setConfirm(false)}>Keep it running</button>
          </div>
        </Modal>
      )}
    </>
  );
}

function RetryRunButton({ run }: { run: TjRun }) {
  const qc = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const recovery = useQuery({ queryKey: ['recovery', run.id], queryFn: () => api.recovery(run.id) });
  const retry = useMutation({
    mutationFn: (ack: boolean) => api.retry(run.id, ack),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['tree', run.id] });
      qc.invalidateQueries({ queryKey: ['recovery', run.id] });
      qc.invalidateQueries({ queryKey: ['questions', run.id] });
      setConfirm(false);
    },
  });
  if (run.status !== 'interrupted') return null;
  const unknown = Boolean(recovery.data?.recovery?.effects_unknown);
  return (
    <>
      <button className="btn btn-sm btn-go" title="Start a new attempt from durable state" onClick={(e) => { e.stopPropagation(); setConfirm(true); }}>Retry</button>
      {confirm && (
        <Modal title="Retry interrupted run?" onClose={() => setConfirm(false)}>
          <p className="tiny dim" style={{ margin: '0 0 12px' }}>
            The previous attempt is fenced. Durable answers and artifacts stay; a <strong>new attempt</strong> re-runs
            the interrupted flow. No model, tool or publication action is replayed automatically.
          </p>
          {unknown && <p className="tiny" style={{ color: 'var(--wait)' }}>
            The interruption may have left an external effect (for example a push or pull request). Inspect the
            activity/effects first; confirming records that you accept the explicit reconciliation.
          </p>}
          {retry.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>{(retry.error as Error).message}</p>}
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-go" disabled={retry.isPending} onClick={() => retry.mutate(unknown)}>
              Start new attempt
            </button>
            <button className="btn" onClick={() => setConfirm(false)}>Keep interrupted</button>
          </div>
        </Modal>
      )}
    </>
  );
}

function InterruptedQuestions({ runId }: { runId: string }) {
  const open = useQuery({ queryKey: ['questions', runId, 'interrupted'], queryFn: () => api.questions(runId, true) });
  const questions = open.data?.questions ?? [];
  if (!questions.length) return null;
  return (
    <div style={{ marginTop: 6 }}>
      <p className="tiny" style={{ margin: 0 }}>Pending question(s) from the interrupted attempt are preserved for review:</p>
      {questions.map((q) => (
        <p key={q.id} className="tiny dim" style={{ margin: '3px 0 0' }}>
          <strong>{q.stage_name}</strong> (attempt {q.attempt + 1}): {q.request.question}
        </p>
      ))}
    </div>
  );
}

function fmtMs(ms: number) {
  if (!Number.isFinite(ms) || ms <= 0) return '0m';
  const minutes = Math.floor(ms / 60000);
  return minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`;
}

function BudgetRow({ label, used, limit, remaining }: { label: string; used: number; limit: number; remaining: number }) {
  const pct = limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 100;
  return (
    <div style={{ marginBottom: 7 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 6 }} className="tiny">
        <span>{label}</span>
        <span className="dim">{used}/{limit} · {remaining} left</span>
      </div>
      <div style={{ height: 4, background: 'var(--sunk)', borderRadius: 3, overflow: 'hidden' }}>
        <div style={{ height: '100%', width: `${pct}%`, background: pct >= 100 ? 'var(--fail)' : pct >= 80 ? 'var(--wait)' : 'var(--run)' }} />
      </div>
    </div>
  );
}

function BudgetPanel({ runId }: { runId: string }) {
  const budget = useQuery({ queryKey: ['budget', runId], queryFn: () => api.budget(runId), refetchInterval: 5000 });
  if (budget.isLoading) return <p className="tiny dim">Loading budget…</p>;
  if (budget.error || !budget.data) return <p className="tiny dim">No durable budget record (legacy run).</p>;
  const b: RunBudget = budget.data;
  return (
    <div>
      {b.exhausted && (
        <div className="banner" style={{ marginBottom: 8, borderColor: 'var(--fail-line)', color: 'var(--fail)' }}>
          <span>Budget exhausted: {b.exhausted_reason || 'limit reached'}. Remaining facts stay unknown; no answers are invented.</span>
        </div>
      )}
      <BudgetRow label="Questions" used={b.question_rounds_used} limit={b.question_rounds_limit} remaining={b.question_rounds_remaining} />
      <BudgetRow label="Model calls" used={b.model_calls_used} limit={b.model_calls_limit} remaining={b.model_calls_remaining} />
      <BudgetRow label="Tool calls" used={b.tool_calls_used} limit={b.tool_calls_limit} remaining={b.tool_calls_remaining} />
      <BudgetRow label="Output corrections" used={b.output_corrections_used} limit={b.output_corrections_limit} remaining={b.output_corrections_remaining} />
      <BudgetRow label="Active time" used={b.active_ms_used} limit={b.active_ms_limit} remaining={b.active_ms_remaining} />
      {b.cost_micros_limit > 0 && (
        <BudgetRow label="Model cost (micros)" used={b.cost_micros_used} limit={b.cost_micros_limit} remaining={b.cost_micros_remaining} />
      )}
      <p className="tiny dim" style={{ margin: '4px 0 0' }}>
        Active {fmtMs(b.active_ms_used)} of {fmtMs(b.active_ms_limit)} · questions per stage ≤ {b.question_rounds_per_stage_limit}
        {b.unknown_usage_calls > 0 ? ` · ${b.unknown_usage_calls} call(s) with unknown provider usage` : ''}
      </p>
    </div>
  );
}

function costLabel(micros: number, calls: number) {
  if (!calls) return 'no calls';
  if (!micros) return 'cost unknown';
  return `${micros.toLocaleString()} micros`;
}

function DiagnosticsPanel({ runId }: { runId: string }) {
  const diag = useQuery({ queryKey: ['diagnostics', runId], queryFn: () => api.diagnostics(runId), refetchInterval: 10000 });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (diag.isLoading) return <p className="tiny dim">Loading diagnostics…</p>;
  if (diag.error || !diag.data) return <p className="tiny dim">Diagnostics unavailable: {(diag.error as Error)?.message || 'no data'}</p>;
  const d = diag.data;
  const exportSupport = async () => {
    setBusy(true); setError(null);
    try { await api.downloadSupportExport(runId); }
    catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  return (
    <div>
      <div className="tiny" style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 3 }}>
        <span>Provider calls</span><span className="dim">{d.provider_usage.calls}</span>
        <span>Usage known / unknown</span><span className="dim">{d.provider_usage.known_usage_calls} / {d.provider_usage.unknown_usage_calls}</span>
        <span>Tokens in / out</span><span className="dim">{d.provider_usage.input_tokens} / {d.provider_usage.output_tokens}</span>
        <span>Cost</span><span className="dim">{costLabel(d.provider_usage.cost_micros, d.provider_usage.calls)}</span>
        <span>Rate-limit / transient retries</span><span className="dim">{d.provider_attempts.rate_limited} / {d.provider_attempts.transient}</span>
        <span>Credential failures</span><span className="dim">{d.provider_attempts.credential}</span>
        <span>Model calls (budget)</span><span className="dim">{d.corrections.llm_calls}</span>
        <span>Corrections / nudges</span><span className="dim">{d.corrections.output_corrections} / {d.corrections.silent_turn_nudges}</span>
        <span>Retention</span><span className="dim">{d.retention_days} days</span>
      </div>
      {d.routes.length > 0 && (
        <>
          <p className="label" style={{ marginTop: 9 }}>Reviewed packet routes</p>
          {d.routes.slice(-3).map((route, index) => (
            <p key={`${route.created_at}-${index}`} className="tiny dim" style={{ margin: '2px 0', wordBreak: 'break-all' }}>
              {route.gate_name}: {route.approved ? 'approved' : 'changes requested'} · revision {route.packet_revision_id?.slice(0, 12) || '—'} · {route.packet_content_hash?.slice(0, 12) || '—'}
            </p>
          ))}
        </>
      )}
      <button className="btn btn-sm" type="button" disabled={busy} onClick={exportSupport} style={{ marginTop: 8 }}>
        {busy ? 'Preparing…' : 'Download credential-safe support export'}
      </button>
      {error && <p className="tiny" style={{ color: 'var(--fail)' }}>{error}</p>}
    </div>
  );
}

function PublicationPanel({ runId }: { runId: string }) {
  const q = useQuery({ queryKey: ['publication', runId], queryFn: () => api.publication(runId), refetchInterval: 5000 });
  if (q.isLoading) return <p className="tiny dim">Loading publication…</p>;
  if (q.error || !q.data) return <p className="tiny dim">No remote publication yet (local workspace only).</p>;
  const publication = q.data.publication;
  if (!publication) return <p className="tiny dim">No remote publication yet — Engineering has not published an approved task.</p>;
  const stateChip = (state: string) => (
    <span className={`chip ${state === 'succeeded' ? 'c-done' : state === 'failed' ? 'c-fail' : 'c-wait'}`}>{state}</span>
  );
  const violations = (() => { try { return JSON.parse(publication.scope_violations_json) as string[]; } catch { return []; } })();
  return (
    <div>
      <p className="tiny" style={{ margin: '0 0 4px' }}>
        Task <span className="mono">{publication.task_id}</span> · branch <span className="mono">{publication.branch}</span>
      </p>
      <p className="tiny dim" style={{ margin: '0 0 4px', wordBreak: 'break-all' }}>
        commit <span className="mono">{publication.commit_sha?.slice(0, 12) || '—'}</span>
        {' · '}plan <span className="mono">{publication.plan_content_hash.slice(0, 12)}</span>
        {' · '}packet <span className="mono">{publication.packet_content_hash.slice(0, 12)}</span>
      </p>
      <div className="tiny" style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 3 }}>
        <span>Push</span><span>{stateChip(publication.push_state)}</span>
        <span>Draft PR</span><span>{stateChip(publication.pr_state)}</span>
      </div>
      {publication.remote_sha && <p className="tiny dim" style={{ margin: '3px 0 0' }}>verified remote SHA <span className="mono">{publication.remote_sha.slice(0, 12)}</span></p>}
      {publication.pr_url && <a className="tiny" href={publication.pr_url} target="_blank" rel="noreferrer">Draft PR #{publication.pr_number} (humans merge)</a>}
      {publication.push_error && <p className="tiny" style={{ color: 'var(--fail)', margin: '3px 0 0' }}>{publication.push_error}</p>}
      {publication.pr_error && <p className="tiny" style={{ color: 'var(--fail)', margin: '3px 0 0' }}>{publication.pr_error}</p>}
      {q.data.reconciliation_required && (
        <p className="tiny" style={{ color: 'var(--wait)', margin: '3px 0 0' }}>
          Remote state is not proven; a retry reconciles with the remote before any repeated push/PR. The workspace is retained.
        </p>
      )}
      {violations.length > 0 && <p className="tiny" style={{ color: 'var(--fail)' }}>Scope violations: {violations.join(', ')}</p>}
    </div>
  );
}

function DeleteRunButton({ run, onDeleted }: { run: TjRun; onDeleted?: () => void }) {
  const qc = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const del = useMutation({
    mutationFn: () => api.deleteRun(run.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['tree', run.id] });
      onDeleted?.();
    },
  });
  if (!TERMINAL.includes(run.status)) return null;
  return (
    <>
      <button className="btn btn-sm btn-no" title="Delete this run" onClick={(e) => { e.stopPropagation(); setConfirm(true); }}>Delete</button>
      {confirm && (
        <Modal title="Delete this run?" onClose={() => setConfirm(false)}>
          <p className="tiny dim" style={{ margin: '0 0 12px' }}>
            Permanently deletes <strong>{run.description || run.id}</strong> — logs, artifacts, decisions and all run files.
            This cannot be undone.
          </p>
          {del.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>{(del.error as Error).message}</p>}
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-no" disabled={del.isPending} onClick={() => del.mutate()}>Delete permanently</button>
            <button className="btn" onClick={() => setConfirm(false)}>Keep it</button>
          </div>
        </Modal>
      )}
    </>
  );
}

export function RunsList({ nav }: { nav: Nav }) {
  const runs = useQuery({ queryKey: ['runs'], queryFn: api.runs });
  const [filter, setFilter] = useState<Filter>('all');
  const [q, setQ] = useState('');

  const list = useMemo(() => {
    let l = runs.data || [];
    if (nav.projectId) l = l.filter((r) => r.project_id === nav.projectId || !r.project_id);
    if (filter === 'active') l = l.filter((r) => !['completed', 'failed', 'cancelled', 'interrupted'].includes(r.status));
    if (filter === 'done') l = l.filter((r) => r.status === 'completed');
    if (filter === 'failed') l = l.filter((r) => ['failed', 'cancelled', 'interrupted'].includes(r.status));
    if (q) l = l.filter((r) => r.description.toLowerCase().includes(q.toLowerCase()) || r.id.toLowerCase().includes(q.toLowerCase()));
    return l;
  }, [runs.data, filter, q, nav.projectId]);

  const chips: [Filter, string][] = [['all', 'All'], ['active', 'In flight'], ['done', 'Finished'], ['failed', 'Failed']];

  return (
    <>
      <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'center', marginBottom: 8 }}>
        <h2 style={{ margin: 0 }}>Runs</h2>
        {nav.projectId && <span className="tag">{nav.projectId}</span>}
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 7, flexWrap: 'wrap' }}>
          {chips.map(([f, label]) => (
            <button key={f} className="fchip" aria-pressed={filter === f} onClick={() => setFilter(f)}>{label}</button>
          ))}
          <input placeholder="Search…" value={q} onChange={(e) => setQ(e.target.value)} style={{ fontSize: 12, padding: '4px 8px', maxWidth: 160 }} />
        </span>
      </div>
      <div className="card">
        {list.map((r, ri) => (
          <div key={r.id} className="runrow" style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '0 12px 0 0', borderBottom: ri < list.length - 1 ? '1px solid var(--rule)' : undefined }}>
            <button className="rowbtn" style={{ flex: 1, minWidth: 0, borderBottom: 0 }} onClick={() => nav.openRun(r.id)}>
              <Dot status={r.status} />
              <span style={{ flex: 1, minWidth: 0 }}>
                <span className="rowtitle">{r.description || r.id}</span>
                <span className="tiny dim" style={{ display: 'block' }}>
                  {new Date(r.created_at).toLocaleString()}
                  {r.pr_url ? <> · <a href={r.pr_url} target="_blank" rel="noreferrer">PR #{r.pr_number}</a></> : null}
                </span>
              </span>
              <StatusChip status={r.status} />
            </button>
            <CancelRunButton run={r} />
            <DeleteRunButton run={r} />
          </div>
        ))}
        {!list.length && <div className="pad tiny dim">No runs match.</div>}
      </div>
    </>
  );
}

function Modal({ title, children, onClose, wide }: { title: string; children: React.ReactNode; onClose: () => void; wide?: boolean }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  return (
    <div className="tw" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="tw-box" role="dialog" aria-modal="true" style={wide ? { width: 'min(980px, 94vw)' } : undefined}>
        <h2>{title}</h2>
        {children}
      </div>
    </div>
  );
}

function pretty(v: unknown): string {
  if (v === null || v === undefined) return '';
  return typeof v === 'string' ? v : JSON.stringify(v, null, 2);
}

function relTime(iso: string | null): string {
  if (!iso) return '';
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return `${Math.floor(s)}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

function duration(start: string | null, end: string | null): string {
  if (!start || !end) return '';
  const s = Math.round((new Date(end).getTime() - new Date(start).getTime()) / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

function clockTime(iso: string | null): string {
  if (!iso) return '';
  const d = new Date(iso);
  const hm = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (d.toDateString() === new Date().toDateString()) return hm;
  return `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${hm}`;
}

/// Decision-ready gate screen: the ask, what happened, what the agents
/// produced, and the actions — everything needed to approve or reject well.
function GateReview({ run, flows, decisions, showAll }: { run: TjRun; flows: TjFlow[]; decisions: GateDecision[]; showAll: boolean }) {
  const stale = supersededFlowIds(flows);
  const groups = flows.flatMap((f) => groupStages(f.stages || []).map((g) => ({ g, flow: f, stale: stale.has(f.id) })));
  const stages = groups.filter(({ stale }) => !stale).map(({ g, flow }) => ({ st: g.latest, flow, superseded: g.earlier.length }));
  return (
    <div>
      <h2 style={{ margin: '0 0 8px' }}>Approval gate</h2>

      <div className="panel" style={{ margin: '0 0 12px', flexShrink: 0 }}>
        <p style={{ margin: 0, whiteSpace: 'pre-wrap' }}>{run.gate_state || 'Approval required to continue.'}</p>
      </div>

      <PacketReview runId={run.id} />
      <PlanReview runId={run.id} hideWhenMissing />

      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', flexShrink: 0, marginBottom: 12 }}>
        <GateActions run={run} />
      </div>

      <div>
        <p className="label" style={{ marginTop: 0 }}>What happened</p>
        {groups.flatMap(({ g, flow, stale }) => showAll ? [...g.earlier.map((x) => ({ st: x, flow, stale, superseded: -1 })), { st: g.latest, flow, stale, superseded: g.earlier.length }] : (stale ? [] : [{ st: g.latest, flow, stale, superseded: g.earlier.length }])).map(({ st, flow, stale, superseded }) => (
          <div key={st.id} style={{ display: 'flex', gap: 9, alignItems: 'center', borderBottom: '1px dotted var(--rule)', padding: '4px 0', opacity: stale || superseded === -1 ? 0.6 : 1 }}>
            <span className="tstamp" style={{ width: 44 }}>{clockTime(st.started_at)}</span>
            <Dot status={st.status} />
            <span style={{ flex: 1, minWidth: 0 }}>
              <strong style={{ fontSize: 12.5 }}>{st.stage_name}</strong>
              <span className="tiny dim"> · {flow.flow_name}{st.iteration > 0 ? ` · attempt ${st.iteration + 1}` : ''}</span>
              {st.error && <span className="tiny" style={{ color: 'var(--fail)' }}> · {st.error.slice(0, 80)}</span>}
              {stale && <span className="tiny dim"> · superseded execution</span>}
              {superseded === -1 && <span className="tiny dim"> · superseded</span>}
              {superseded > 0 && <span className="tiny dim"> · {superseded} earlier attempt{superseded === 1 ? '' : 's'}</span>}
            </span>
            <span className="tstamp">{duration(st.started_at, st.completed_at)}</span>
          </div>
        ))}
        {!stages.length && <p className="tiny dim">No stage activity recorded.</p>}

        {stages.length > 0 && <p className="label" style={{ marginTop: 14 }}>What the agents produced</p>}
        {stages.map(({ st }) => {
          const out = st.output_artifacts as Record<string, unknown> | null;
          if (!out) return null;
          const md = typeof out['output.md'] === 'string' ? out['output.md'] : null;
          const extras = Object.entries(out).filter(([k]) => k !== 'output.md');
          if (!md && !extras.length) return null;
          return (
            <details key={st.id} style={{ marginBottom: 8 }}>
              <summary className="rowbtn" style={{ padding: '6px 10px', gap: 8, width: 'auto' }}>
                <span className="acc">▸</span>
                <span className="chip c-done">{st.stage_name}</span>
                <span className="tiny dim" style={{ flex: 1, textAlign: 'left' }}>output</span>
              </summary>
              <div style={{ padding: '4px 2px' }}>
                {md && <div className="md-body" style={{ border: '1px solid var(--rule)', borderRadius: 'var(--r)', padding: '10px 14px' }}><MarkdownView text={md} /></div>}
                {extras.map(([k, v]) => (
                  <div key={k} style={{ padding: '6px 2px' }}>
                    <p className="label" style={{ margin: '2px 0' }}>{k.replace(/[_.]/g, ' ')}</p>
                    <div className="md-body tiny">{typeof v === 'string' ? v : JSON.stringify(v)}</div>
                  </div>
                ))}
              </div>
            </details>
          );
        })}

        <p className="label" style={{ marginTop: 14 }}>Decisions so far</p>
        {decisions.length ? decisions.map((d) => (
          <div key={d.id} className="evt">
            <span className="tstamp" style={{ width: 44 }}>{d.created_at.slice(11, 16)}</span>
            <span className={`k ${d.approved ? 'k-ok' : 'k-fail'}`} />
            <span style={{ flex: 1, minWidth: 0 }}>
              {d.approved ? 'approved' : 'rejected'} by {d.actor_name}
              {d.reason && <span className="tiny dim"> — {d.reason}</span>}
            </span>
          </div>
        )) : <p className="tiny dim">None yet — this is the first.</p>}
      </div>
    </div>
  );
}

function GateActions({ run }: { run: TjRun }) {
  return <GatePanel run={run} />;
}

function newIdempotencyKey(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  return `gate-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function GatePanel({ run }: { run: TjRun }) {
  const qc = useQueryClient();
  // Durable normalized identity is the only panel selector. There is no
  // profile or gate-name fallback: a run without a binding kind shows no
  // domain panel rather than guessing from business names.
  const bindingKind = run.gate_binding?.kind ?? null;
  const genericGate = bindingKind === 'artifact';
  const packetGate = bindingKind === 'packet';
  const planningGate = bindingKind === 'plan';
  // A plan-bound gate also carries its approved source packet.
  const bindsPacket = packetGate || planningGate;
  const packetQ = useQuery({ queryKey: ['packet', run.id], queryFn: () => api.packet(run.id), enabled: bindsPacket });
  const planQ = useQuery({ queryKey: ['plan', run.id], queryFn: () => api.plan(run.id), enabled: planningGate });
  const outputQ = useQuery({ queryKey: ['output', run.id], queryFn: () => api.output(run.id), enabled: genericGate });
  const binding = packetQ.data?.gate_binding ?? null;
  const packet = packetQ.data?.latest ?? null;
  const planBinding = planQ.data?.gate_binding ?? null;
  const plan = planQ.data?.latest ?? null;
  const boundPlan = plan && planBinding && planBinding.revision_id === plan.id ? plan : null;
  const outputBinding = outputQ.data?.gate_binding ?? null;
  const boundOutput = outputBinding && outputQ.data?.latest && outputBinding.artifact_id === outputQ.data.latest.id
    ? outputQ.data.latest
    : null;
  const boundPacket = packet && binding && binding.revision_id === packet.id ? packet : null;
  const needsAcknowledgement = Boolean(boundPacket?.completeness === 'incomplete');
  const contradictory = Boolean(boundPacket?.completeness === 'contradictory');
  const [acknowledgeIncomplete, setAcknowledgeIncomplete] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const [reason, setReason] = useState<string>(REJECT_REASONS[0]);
  const [error, setError] = useState<string | null>(null);
  const [accepted, setAccepted] = useState<string | null>(null);
  const act = useMutation({
    mutationFn: (args: { approved: boolean; reason?: string }) => {
      if (!run.gate_instance_id) throw new Error('This gate has no active instance identity; refresh the run before deciding.');
      return api.approve(run.id, {
        approved: args.approved,
        reason: args.reason,
        gate_instance_id: run.gate_instance_id,
        expected_state_version: run.state_version,
        idempotency_key: newIdempotencyKey(),
        packet_revision_id: binding?.revision_id,
        packet_content_hash: binding?.content_hash,
        plan_revision_id: planBinding?.revision_id,
        plan_content_hash: planBinding?.content_hash,
        output_artifact_id: outputBinding?.artifact_id ?? undefined,
        output_content_hash: outputBinding?.content_hash ?? undefined,
        output_stage_name: outputBinding?.stage_name ?? undefined,
        acknowledge_incomplete: acknowledgeIncomplete,
      });
    },
    onSuccess: (receipt) => {
      setError(null);
      setAccepted(`${receipt.disposition.replaceAll('_', ' ')} — run is ${receipt.status.replaceAll('_', ' ')} (state ${receipt.state_version}${receipt.replayed ? ', replayed' : ''}${receipt.output_artifact_id ? `, output ${receipt.output_artifact_id.slice(0, 14)}… @ ${receipt.output_content_hash?.slice(0, 12)}…` : ''})`);
      qc.invalidateQueries({ queryKey: ['runs'] }); qc.invalidateQueries({ queryKey: ['tree', run.id] }); qc.invalidateQueries({ queryKey: ['decisions', run.id] }); qc.invalidateQueries({ queryKey: ['events', run.id] }); qc.invalidateQueries({ queryKey: ['artifacts', run.id] }); qc.invalidateQueries({ queryKey: ['explorer-trees'] }); },
    onError: (e) => { setAccepted(null); setError((e as Error).message); },
  });
  const [cancelOpen, setCancelOpen] = useState(false);
  const cancel = useMutation({
    mutationFn: () => api.cancel(run.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['tree', run.id] });
      qc.invalidateQueries({ queryKey: ['decisions', run.id] });
      qc.invalidateQueries({ queryKey: ['explorer-trees'] });
    },
  });

  return (
    <>
      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
        <button
          className="btn btn-go"
          disabled={act.isPending || contradictory || (needsAcknowledgement && !acknowledgeIncomplete) || (packetGate && !boundPacket) || (planningGate && !boundPlan) || (genericGate && !boundOutput)}
          onClick={() => act.mutate({ approved: true })}
        >Approve</button>
        <button className="btn btn-no" disabled={act.isPending} onClick={() => setRejectOpen(true)}>Reject with cause</button>
      </div>
      {needsAcknowledgement && (
        <label className="tiny" style={{ display: 'flex', gap: 7, alignItems: 'center', color: 'var(--wait)' }}>
          <input type="checkbox" checked={acknowledgeIncomplete} onChange={(event) => setAcknowledgeIncomplete(event.target.checked)} />
          This packet is incomplete. I reviewed the open unknowns/decisions and approve this exact revision knowingly.
        </label>
      )}
      {contradictory && (
        <p className="tiny" role="alert" style={{ color: 'var(--fail)' }}>
          Approval is blocked while the bound packet revision contains unresolved contradictions. Save a revised packet or request changes first.
        </p>
      )}
      {genericGate && (
        <div style={{ border: '1px solid var(--rule)', borderRadius: 'var(--r)', padding: '8px 10px', margin: '8px 0' }}>
          <p className="label" style={{ marginTop: 0 }}>Exact output under review</p>
          {boundOutput ? (
            <>
              <p className="tiny" style={{ margin: '0 0 4px' }}>
                <strong>{outputBinding?.stage_name}</strong> · {outputBinding?.flow_name} · artifact {outputBinding?.artifact_id}
              </p>
              <p className="tiny dim mono" style={{ margin: 0, wordBreak: 'break-all' }}>{outputBinding?.content_hash}</p>
              <details style={{ marginTop: 6 }}>
                <summary className="tiny">View reviewed output</summary>
                <pre className="tiny" style={{ whiteSpace: 'pre-wrap', maxHeight: 240, overflow: 'auto' }}>{boundOutput.content}</pre>
              </details>
            </>
          ) : (
            <p className="tiny" role="alert" style={{ color: 'var(--wait)', margin: 0 }}>
              No exact output artifact is bound yet. Approval is blocked until the produced output is durable; refresh the run.
            </p>
          )}
        </div>
      )}
      {packetGate && !boundPacket && (
        <p className="tiny" role="alert" style={{ color: 'var(--wait)' }}>
          This requirements-packet gate has no bound packet revision. Refresh after packet generation; approval cannot be recorded without exact packet content.
        </p>
      )}
      {planningGate && !boundPlan && (
        <p className="tiny" role="alert" style={{ color: 'var(--wait)' }}>
          This plan gate has no bound structured plan revision. Approval is blocked until deterministic validation produces a plan bound to the exact approved packet.
        </p>
      )}
      {planningGate && boundPlan?.validation?.requires_packet_revision && (
        <p className="tiny" role="alert" style={{ color: 'var(--wait)' }}>
          The plan records scope expansion that needs a new approved requirements packet revision. Those items are not tasks and cannot be selected for the target flow.
        </p>
      )}
      {error && <p className="tiny" style={{ color: 'var(--fail)' }}>{error}</p>}
      {accepted && <p className="tiny" style={{ color: 'var(--done)' }}>{accepted}</p>}
      <p className="tiny dim" style={{ marginTop: 9 }}>
        {packetGate
          ? 'Approving records the exact packet revision/hash shown above. Rejecting sends the run back for rework; the rejected revision and cause stay in history.'
          : genericGate
            ? 'Approving records the exact output artifact ID and content hash shown above. Rejecting sends this workflow back for revision; after 3 rejections the run is cancelled.'
            : planningGate
            ? 'Approving binds the exact plan and source packet revisions before the target flow. Rejecting sends Planning back for bounded rework; plan edits while this gate is open make an earlier approval stale.'
            : 'Approving continues to the next flow. Rejecting sends the run back to the previous flow for rework — the cause drives it, and the decision is recorded against your account.'}
      </p>
      {rejectOpen && (
        <Modal title="Reject with cause" onClose={() => setRejectOpen(false)}>
          <p className="tiny dim" style={{ margin: '0 0 12px' }}>
            Rejection sends this run back to the previous flow for rework. Cancellation (stopping without rework) is a separate action.
            After 3 rejections per run, the run is cancelled.
          </p>
          <div className="field">
            <label htmlFor="why">Cause</label>
            <select id="why" value={reason} onChange={(e) => setReason(e.target.value)}>
              {REJECT_REASONS.map((r) => <option key={r}>{r}</option>)}
            </select>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-no" onClick={() => { act.mutate({ approved: false, reason }); setRejectOpen(false); }}>
              Reject — send back for rework
            </button>
            <button className="btn" onClick={() => setRejectOpen(false)}>Keep waiting</button>
          </div>
        </Modal>
      )}
      {!['completed', 'failed', 'cancelled'].includes(run.status) && (
        <p className="tiny dim" style={{ marginTop: 6 }}>
          <button className="btn btn-sm btn-no" onClick={() => setCancelOpen(true)}>Cancel run</button>{' '}
          terminal stop — distinct from rejecting a gate
        </p>
      )}
      {cancelOpen && (
        <Modal title="Cancel this run?" onClose={() => setCancelOpen(false)}>
          <p className="tiny dim" style={{ margin: '0 0 12px' }}>
            The run will be stopped — terminal, no rework. This is distinct from rejecting a gate, which sends work back.
          </p>
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-no" onClick={() => { cancel.mutate(); setCancelOpen(false); }}>Cancel run</button>
            <button className="btn" onClick={() => setCancelOpen(false)}>Keep it running</button>
          </div>
        </Modal>
      )}
    </>
  );
}

/** Renders artifact content by type: JSON pretty, markdown-ish text, or raw. */
function ArtifactView({ content }: { content: string }) {
  const asJson = useMemo(() => {
    try { return JSON.parse(content) as unknown; } catch { return null; }
  }, [content]);
  if (asJson && typeof asJson === 'object') {
    return <pre className="term" style={{ maxHeight: 'none' }}>{JSON.stringify(asJson, null, 2)}</pre>;
  }
  return <div style={{ whiteSpace: 'pre-wrap', lineHeight: 1.55, fontSize: 13.5 }}>{content}</div>;
}

function ArtifactsPanel({ runId, selection, flows }: { runId: string; selection: Selection; flows: TjFlow[] }) {
  const qc = useQueryClient();
  const artifacts = useQuery({ queryKey: ['artifacts', runId], queryFn: () => api.artifacts(runId) });
  const fork = useMutation({
    mutationFn: (aid: string) => api.forkArtifact(aid),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['artifacts', runId] }),
  });
  const [open, setOpen] = useState<TjArtifact | null>(null);
  const list = artifacts.data || [];
  const latest: Record<string, TjArtifact> = {};
  for (const a of list) {
    const key = `${a.flow_name}/${a.stage_name}`;
    if (!latest[key] || a.version > latest[key].version) latest[key] = a;
  }
  let rows = Object.values(latest).sort((a, b) => a.flow_name.localeCompare(b.flow_name) || a.stage_name.localeCompare(b.stage_name));
  if (selection.kind === 'flow') {
    rows = rows.filter((a) => a.flow_name === selection.name);
  } else if (selection.kind === 'stage') {
    const found = flows.flatMap((f) => f.stages.map((st) => ({ st, flowName: f.flow_name }))).find((x) => x.st.id === selection.id);
    if (found) rows = rows.filter((a) => a.stage_name === found.st.stage_name && a.flow_name === found.flowName);
  }
  return (
    <>
      {artifacts.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>Failed to load artifacts.</p>}
      {rows.length ? rows.map((a) => (
        <div key={a.id} style={{ borderBottom: '1px solid var(--rule)', padding: '10px 0' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
            <span className="mono" style={{ fontWeight: 600 }}>{a.flow_name} / {a.stage_name}</span>
            <span className="tiny dim">v{a.version} · attempt {a.attempt + 1} · {relTime(a.created_at)}</span>
            <span style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
              <button className="btn" onClick={() => setOpen(a)}>View</button>
              <button className="btn" onClick={() => fork.mutate(a.id)} title="Create a new immutable version from this artifact">Fork</button>
            </span>
          </div>
          <p className="tiny dim" style={{ margin: '4px 0 0', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
            {pretty(a.content).slice(0, 140) || a.content.slice(0, 140)}
          </p>
        </div>
      )) : (!artifacts.isError && <p className="tiny dim">No artifacts stored yet — completed stages are stored automatically.</p>)}
      {fork.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>Fork failed: {(fork.error as Error)?.message || 'unknown error'}</p>}
      {open && (
        <Modal wide title={`${open.flow_name} / ${open.stage_name} · v${open.version}`} onClose={() => setOpen(null)}>
          <p className="tiny dim" style={{ margin: '0 0 8px' }}>attempt {open.attempt + 1} · sha {open.content_hash.slice(0, 12)} · {new Date(open.created_at).toLocaleString()}</p>
          <div>
            <ArtifactView content={open.content} />
          </div>
        </Modal>
      )}
    </>
  );
}

function FilesPanel({ runId }: { runId: string }) {
  const files = useQuery({ queryKey: ['files', runId], queryFn: () => api.runFiles(runId), refetchInterval: 5000 });
  const [preview, setPreview] = useState<string | null>(null);
  if (files.isLoading) return <p className="tiny dim">Listing workspace files…</p>;
  if (files.error) return <p className="tiny" style={{ color: 'var(--fail)' }}>Failed to list files: {(files.error as Error).message}</p>;
  const list = files.data?.files ?? [];
  return (
    <div>
      {!list.length && <p className="tiny dim">No files in the run workspace yet.</p>}
      {list.map((f) => (
        <div key={f} className="evt">
          <span className="mono tiny" style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>{f}</span>
          <button className="btn btn-sm" onClick={() => setPreview(f)}>Preview</button>
        </div>
      ))}
      {preview && (
        <div className="tw" onClick={(e) => e.target === e.currentTarget && setPreview(null)}>
          <div className="tw-box" role="dialog" aria-modal="true" style={{ width: 'min(860px, 94vw)', maxHeight: '88vh', display: 'flex', flexDirection: 'column' }}>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 10 }}>
              <span className="mono tiny" style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>{preview}</span>
              <button className="btn btn-sm" onClick={() => setPreview(null)}>Close</button>
            </div>
            <div style={{ overflow: 'auto', minHeight: 0, flex: 1 }}>
              <FilePreview runId={runId} path={preview} />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

const LOG_KIND_META: Record<string, { label: string; cls: string }> = {
  llm_request: { label: '→ prompt', cls: 'run' },
  llm_response: { label: '← response', cls: 'ok' },
  tool_call: { label: 'tool', cls: 'wait' },
  tool_result: { label: 'result', cls: 'info' },
};

function LogEntry({ e }: { e: StageLog }) {
  const [open, setOpen] = useState(false);
  const meta = LOG_KIND_META[e.kind] || { label: e.kind, cls: 'info' };
  const oneLine = e.content.split('\n').find((l) => l.trim())?.slice(0, 150) || '';
  return (
    <div
      style={{
        border: '1px solid var(--rule)',
        borderRadius: 'var(--r)',
        marginBottom: 6,
        overflow: 'hidden',
        background: open ? 'var(--sunk)' : 'transparent',
        transition: 'background 160ms ease',
      }}
    >
      <button
        className="rowbtn"
        style={{ padding: '7px 10px', gap: 8, borderBottom: open ? '1px solid var(--rule)' : '1px solid transparent' }}
        onClick={() => setOpen(!open)}
        aria-expanded={open}
      >
        <span className={`acc${open ? ' acc-open' : ''}`}>▸</span>
        <span className={`k k-${meta.cls}`} />
        <span className="tstamp mono" style={{ width: 60 }}>{e.created_at.slice(11, 19)}</span>
        <span className="chip c-idle" style={{ flexShrink: 0 }}>{e.stage_name || 'stage'}</span>
        <span className="tiny" style={{ flexShrink: 0, width: 74, color: 'var(--muted)' }}>{meta.label}</span>
        <span className="tiny dim" style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textAlign: 'left' }}>
          {oneLine}
        </span>
      </button>
      {open && (
        <pre className="term" style={{ margin: 0, border: 0, borderRadius: 0, background: 'transparent', maxHeight: 'none', overflow: 'visible', fontSize: 12 }}>
          {e.content || '(empty)'}
        </pre>
      )}
    </div>
  );
}

function ActivityPanel({ runId, active, selection, flows }: { runId: string; active: boolean; selection: Selection; flows: TjFlow[] }) {
  const stageIds = useMemo(() => {
    if (selection.kind === 'run') return null; // no filter
    if (selection.kind === 'stage') return new Set([selection.id]);
    if (selection.kind !== 'flow') return new Set<string>(); // gate: nothing technical to log
    const flow = flows.find((f) => f.flow_name === selection.name);
    return new Set((flow?.stages || []).map((s) => s.id));
  }, [selection, flows]);
  const [logs, setLogs] = useState<StageLog[]>([]);
  const [error, setError] = useState<string | null>(null);
  const visible = stageIds ? logs.filter((l) => stageIds.has(l.stage_id)) : logs;
  const afterRef = useRef(0);

  useEffect(() => {
    afterRef.current = 0;
    setLogs([]);
    setError(null);
    let stop = false;
    let busy = false;
    const seen = new Set<number>();
    const tick = async () => {
      if (busy) return;
      busy = true;
      try {
        const rows = await api.stageLogs(runId, afterRef.current);
        if (stop) return;
        setError(null);
        const fresh = rows.filter((r) => !seen.has(r.id));
        if (fresh.length) {
          afterRef.current = Math.max(afterRef.current, fresh[fresh.length - 1].id);
          for (const r of fresh) seen.add(r.id);
          setLogs((prev) => [...prev, ...fresh]);
        }
      } catch (e) {
        if (!stop) setError((e as Error).message);
      } finally {
        busy = false;
      }
    };
    tick();
    const interval = setInterval(tick, active ? 1500 : 5000);
    return () => { stop = true; clearInterval(interval); };
  }, [runId, active]);

  return (
    <div>
      {visible.map((e) => <LogEntry key={e.id} e={e} />)}
      {!visible.length && !error && <p className="tiny dim">No agent activity captured yet — it appears here live while stages run.</p>}
      {error && <p className="tiny" style={{ color: 'var(--fail)' }}>Failed to load activity: {error}</p>}
    </div>
  );
}

/// Banner for run/flow scope: points at the agent that is waiting.
function WaitingBanner({ flows, onSelect }: { flows: TjFlow[]; onSelect: (id: string) => void }) {
  const waiting = flows.flatMap((f) => groupStages(f.stages || []).map((g) => g.latest)).find((s) => s.status === 'waiting_for_input');
  const [show, setShow] = useState(true);
  useEffect(() => { setShow(true); }, [waiting?.id]);
  if (!waiting || !show) return null;
  return (
    <div className="banner" style={{ marginBottom: 10 }}>
      <span>An agent is waiting for your input — open its conversation to reply.</span>
      <button className="btn btn-sm btn-go" onClick={() => { onSelect(waiting.id); setShow(false); }}>Open {waiting.stage_name} →</button>
    </div>
  );
}

interface ChatTurn { id: number; kind: 'agent_message' | 'user_input'; content: string; created_at: string }

/// Parse a `question` stage-log row into the structured QuestionRequest (plan
/// T-10). Returns null for anything the user could not answer as a structured
/// round — plain text, wrong shape, or the degenerate no-options + no-free-form
/// ask the runner already rejects — so those fall back to the plain input box.
export function parseQuestionPayload(content: string): QuestionRequest | null {
  let v: unknown;
  try { v = JSON.parse(content); } catch { return null; }
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
  const q = v as Partial<QuestionRequest>;
  if (typeof q.question !== 'string' || !q.question.trim()) return null;
  const rawOptions = Array.isArray(q.options) ? q.options : [];
  if (rawOptions.some((o) => !o || typeof o.id !== 'string' || typeof o.label !== 'string')) return null;
  const freeForm = q.free_form === true;
  if (!rawOptions.length && !freeForm) return null;
  return {
    question: q.question,
    options: rawOptions.map((o) => ({ id: o.id, label: o.label })),
    multi_select: q.multi_select === true,
    free_form: freeForm,
  };
}

/// Question identity carried alongside the request in a `question` log row
/// (TJ-014). Legacy rows without identity stay on the plain-text fallback.
export interface ParsedQuestion {
  questionId: string;
  stageId: string;
  attempt: number;
  status: string;
  request: QuestionRequest;
}

export function parseQuestionRecord(content: string): ParsedQuestion | null {
  const request = parseQuestionPayload(content);
  if (!request) return null;
  let value: unknown;
  try { value = JSON.parse(content); } catch { return null; }
  const raw = value as { question_id?: unknown; stage_id?: unknown; attempt?: unknown; status?: unknown };
  if (typeof raw.question_id !== 'string' || !raw.question_id.trim()) return null;
  if (typeof raw.stage_id !== 'string' || !raw.stage_id.trim()) return null;
  if (typeof raw.attempt !== 'number' || !Number.isInteger(raw.attempt) || raw.attempt < 0) return null;
  return {
    questionId: raw.question_id,
    stageId: raw.stage_id,
    attempt: raw.attempt,
    status: typeof raw.status === 'string' ? raw.status : 'open',
    request,
  };
}

/// Interactive rendering of a pending structured question (plan T-10): the
/// question text, options as checkboxes (multi-select) or radios (single), an
/// optional free-form field, and a Submit button. Submit posts the
/// QuestionAnswer as JSON inside the existing POST /input `text` field (OQ-2) —
/// the runner-side parser detects the JSON object. After submit the input
/// status query is invalidated so the view returns to the live stage.
export function QuestionCard({ runId, stageId, attempt, questionId, stageName, question }: {
  runId: string; stageId: string; attempt: number; questionId: string; stageName: string; question: QuestionRequest;
}) {
  const qc = useQueryClient();
  const idempotencyKey = useRef(newIdempotencyKey());
  const [selected, setSelected] = useState<string[]>([]);
  const [text, setText] = useState('');
  const send = useMutation({
    mutationFn: (answer: { selected: string[]; text?: string }) => api.sendQuestionAnswer(runId, answer, {
      stageId,
      attempt,
      questionId,
      // Stable for this question mount: a lost response can be retried
      // without appending a second answer.
      idempotencyKey: idempotencyKey.current,
    }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['input', runId] });
      qc.invalidateQueries({ queryKey: ['questions', runId] });
      qc.invalidateQueries({ queryKey: ['tree', runId] });
    },
  });
  const toggle = (id: string) => {
    if (question.multi_select) setSelected((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
    else setSelected([id]);
  };
  const trimmed = text.trim();
  const canSubmit = !send.isPending && !send.isSuccess && (selected.length > 0 || (question.free_form && trimmed.length > 0));
  const submit = () => { if (canSubmit) send.mutate({ selected, text: trimmed || undefined }); };

  if (send.isSuccess) {
    return (
      <div className="evt" style={{ marginTop: 8, flexShrink: 0 }}>
        <span className="chip c-done">answer sent</span>
        <span className="tiny dim" style={{ flex: 1 }}>{stageName} is continuing with your answer…</span>
      </div>
    );
  }
  return (
    <div className="panel" style={{ marginTop: 8, flexShrink: 0, display: 'flex', flexDirection: 'column', gap: 8 }}>
      <p style={{ margin: 0, whiteSpace: 'pre-wrap', fontSize: 13.5 }}>{question.question}</p>
      {question.options.map((o) => (
        <label key={o.id} style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 13, cursor: 'pointer' }}>
          <input
            type={question.multi_select ? 'checkbox' : 'radio'}
            name="tj-question-option"
            checked={selected.includes(o.id)}
            onChange={() => toggle(o.id)}
          />
          <span>{o.label}</span>
        </label>
      ))}
      {question.free_form && (
        <textarea
          aria-label="Free-form answer"
          style={{ resize: 'vertical', minHeight: 38, maxHeight: 160, fontFamily: 'inherit' }}
          value={text}
          placeholder={question.options.length ? 'Add details (optional)…' : 'Type your answer…'}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit(); } }}
        />
      )}
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <button className="btn btn-go" disabled={!canSubmit} onClick={submit}>Submit answer</button>
        <span className="tiny dim">
          {question.options.length > 0 && (question.multi_select ? 'Select all that apply.' : 'Pick one.')}
          {question.free_form && ' Free text welcome.'}
        </span>
      </div>
      {send.isError && <p className="tiny" style={{ color: 'var(--fail)', margin: 0 }}>{(send.error as Error).message}</p>}
    </div>
  );
}

/// The conversation with ONE agent: markdown-rendered turns, technical details
/// per turn, queued outgoing messages, live via SSE. Input is bound to the stage.
export function StageChat({ runId, stageId, stageIteration, stageName, stageStatus }: {
  runId: string; stageId: string; stageIteration: number; stageName: string; stageStatus: string;
}) {
  const qc = useQueryClient();
  const userId = qc.getQueryData<{ id: string }>(['me'])?.id;
  const draftKey = userId ? `tj-draft-v2:${userId}:${stageId}` : null;
  const [turns, setTurns] = useState<ChatTurn[]>([]);
  const [questions, setQuestions] = useState<Record<string, ParsedQuestion>>({});
  const [text, setText] = useState(() => draftKey ? localStorage.getItem(draftKey) ?? '' : '');
  const [queued, setQueued] = useState<{ seq: number; text: string }[]>([]);
  const [filter, setFilter] = useState('');
  const [viewer, setViewer] = useState<ViewerArtifact | null>(null);
  const [live, setLive] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const waiting = stageStatus === 'waiting_for_input';

  // Drafts survive switching between agents.
  const setTextDraft = (v: string) => { setText(v); if (draftKey) localStorage.setItem(draftKey, v); };
  useEffect(() => { setText(draftKey ? localStorage.getItem(draftKey) ?? '' : ''); }, [draftKey]);

  // Live stream with automatic poll fallback (inside api.stageLogStream).
  useEffect(() => {
    setTurns([]);
    setQuestions({});
    setQueued([]);
    setExportError(null);
    const seen = new Set<number>();
    const apply = (rows: StageLog[]) => {
      // Run-wide rows are never rendered in a stage conversation (TJ-015).
      const fresh = rows.filter((r) => r.stage_id === stageId && !seen.has(r.id));
      for (const r of fresh) {
        seen.add(r.id);
        if (r.kind === 'agent_message' || r.kind === 'user_input') {
          setTurns((prev) => [...prev, {
            id: r.id, kind: r.kind as ChatTurn['kind'], content: r.content, created_at: r.created_at,
          }].sort((a, b) => a.id - b.id));
        } else if (r.kind === 'question') {
          const q = parseQuestionRecord(r.content);
          if (q) setQuestions((prev) => ({ ...prev, [q.stageId]: q }));
        }
      }
    };
    // Seed from REST once (full history), then stream deltas.
    api.stageLogs(runId, 0).then(apply).catch(() => undefined);
    const stop = api.stageLogStream(runId, (rows) => apply(rows as StageLog[]), undefined, () => setLive(true));
    return stop;
  }, [runId, stageId]);

  // Durable pending question (TJ-014): a reconnect can re-present the open
  // question even when the browser missed the original log stream.
  const openQuestions = useQuery({
    queryKey: ['questions', runId, stageId],
    queryFn: () => api.questions(runId, true),
    enabled: waiting,
    refetchInterval: waiting ? 2500 : false,
  });

  // Queue + waiting state.
  const status = useQuery({ queryKey: ['input', runId], queryFn: () => api.inputStatus(runId), refetchInterval: 2500 });
  useEffect(() => { setQueued(status.data?.queued ?? []); }, [status.data?.queued]);

  const send = useMutation({
    mutationFn: (t: string) => api.sendInput(runId, t, {
      stageId,
      attempt: stageIteration,
      idempotencyKey: newIdempotencyKey(),
    }),
    onSuccess: (_r, t) => {
      setTextDraft('');
      setQueued((q) => [...q, { seq: Date.now(), text: t }]);
      qc.invalidateQueries({ queryKey: ['input', runId] });
      qc.invalidateQueries({ queryKey: ['tree', runId] });
    },
  });
  const cancel = useMutation({
    mutationFn: (seq: number) => api.cancelInput(runId, seq),
    onSuccess: () => { qc.invalidateQueries({ queryKey: ['input', runId] }); },
  });
  const canSend = waiting && text.trim().length > 0 && !send.isPending;

  const visible = filter.trim()
    ? turns.filter((t) => t.content.toLowerCase().includes(filter.trim().toLowerCase()))
    : turns;

  const exportMd = async () => {
    setExportError(null);
    const md = turns.map((t) => (t.kind === 'user_input' ? `**You:**\n\n${t.content}` : `**${stageName}:**\n\n${t.content}`)).join('\n\n---\n\n');
    const header = `# Conversation with ${stageName}\n\nStage: ${stageId} · attempt ${stageIteration + 1}\n\n`;
    try {
      await navigator.clipboard.writeText(`${header}${md}\n`);
    } catch (error) {
      setExportError(`Export failed: ${(error as Error).message || 'clipboard unavailable'}`);
    }
  };

  // A pending structured question replaces the plain input box (plan T-10).
  // The durable API record wins over a possibly-stale log row; a changed
  // question id remounts the card and clears any previous selection/text.
  const apiPending = openQuestions.data?.questions.find((q: StageQuestion) => q.stage_id === stageId);
  const logPending = questions[stageId] ?? null;
  const pendingQuestion: ParsedQuestion | null = waiting
    ? (apiPending
        ? { questionId: apiPending.id, stageId: apiPending.stage_id, attempt: apiPending.attempt, status: apiPending.status, request: apiPending.request }
        : logPending)
    : null;

  return (
    <ViewerContext.Provider value={setViewer}>
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginBottom: 8, flexShrink: 0 }}>
        <input style={{ flex: 1 }} value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Search this conversation…" />
        {live && <span className="chip c-wait">live</span>}
        <button className="tbtn" onClick={() => void exportMd()} title="Copy this stage conversation as markdown">Export</button>
      </div>
      <div style={{ flex: 1, minHeight: 0 }}>
        <Virtuoso
          data={visible}
          followOutput="smooth"
          initialTopMostItemIndex={visible.length ? visible.length - 1 : 0}
          itemContent={(_i, turn) => {
            const mine = turn.kind === 'user_input';
            return (
              <div style={{ display: 'flex', flexDirection: 'column', marginBottom: 10, alignItems: mine ? 'flex-end' : 'flex-start' }} className="chat-turn">
                <div style={{
                  maxWidth: '86%',
                  background: mine ? 'var(--done-bg)' : 'var(--sunk)',
                  border: `1px solid ${mine ? 'var(--done-line)' : 'var(--rule)'}`,
                  borderRadius: mine ? '12px 12px 3px 12px' : '12px 12px 12px 3px',
                  padding: '9px 12px',
                }}>
                  <div className={mine ? '' : 'md-body'} style={{ fontSize: 13 }}>
                    {mine ? turn.content : <MarkdownView text={turn.content} />}
                  </div>
                  <div className="stepmeta" style={{ marginTop: 6, opacity: 0.75 }}>
                    <span className="tstamp">{turn.created_at.slice(11, 19)}</span>
                    <button className="tbtn" onClick={() => navigator.clipboard.writeText(turn.content)}>Copy</button>
                  </div>
                </div>
              </div>
            );
          }}
        >
          {visible.length === 0 && <p className="tiny dim">{stageName} has not asked anything yet. When it does, the question appears here and you reply in this box.</p>}
        </Virtuoso>
      </div>

      {queued.length > 0 && (
        <div style={{ flexShrink: 0, marginTop: 8 }}>
          {queued.map((q) => (
            <div key={q.seq} className="evt">
              <span className="chip c-wait">queued</span>
              <span className="tiny dim" style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{q.text}</span>
              <button className="btn btn-sm" onClick={() => cancel.mutate(q.seq)}>Cancel</button>
            </div>
          ))}
        </div>
      )}

      {exportError && <p className="tiny" style={{ color: 'var(--fail)', flexShrink: 0 }} role="alert">{exportError}</p>}
      {pendingQuestion ? (
        <QuestionCard
          key={pendingQuestion.questionId}
          runId={runId}
          stageId={stageId}
          attempt={pendingQuestion.attempt}
          questionId={pendingQuestion.questionId}
          stageName={stageName}
          question={pendingQuestion.request}
        />
      ) : (
        <>
          {!waiting && stageStatus === 'running' && <p className="tiny dim" style={{ margin: '6px 0 0', flexShrink: 0 }}>{stageName} is working — it will ask here when it needs you.</p>}
          <form style={{ display: 'flex', gap: 7, marginTop: 8, flexShrink: 0 }} onSubmit={(e) => { e.preventDefault(); if (canSend) send.mutate(text.trim()); }}>
            <textarea
              style={{ flex: 1, resize: 'vertical', minHeight: 38, maxHeight: 160, fontFamily: 'inherit' }}
              value={text}
              disabled={!waiting}
              placeholder={waiting ? `Reply to ${stageName}… (Enter to send, Shift+Enter for newline)` : `${stageName} is not asking right now`}
              onChange={(e) => setTextDraft(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); if (canSend) send.mutate(text.trim()); } }}
            />
            <button className="btn btn-go" type="submit" disabled={!canSend}>Send</button>
          </form>
        </>
      )}
      {send.isError && <p className="tiny" style={{ color: 'var(--fail)', flexShrink: 0 }}>{(send.error as Error).message}</p>}
      {cancel.isError && <p className="tiny" style={{ color: 'var(--fail)', flexShrink: 0 }}>{(cancel.error as Error).message}</p>}
      {viewer && <ArtifactViewerPane artifact={viewer} onClose={() => setViewer(null)} />}
    </div>
    </ViewerContext.Provider>
  );
}

type CenterTab = 'chat' | 'logs' | 'artifacts' | 'files' | 'plan';

interface StageGroup { latest: TjStage; earlier: TjStage[] }

/// Flow rows superseded by a later execution of the same flow (resume leftovers).
function supersededFlowIds(flows: TjFlow[]): Set<string> {
  const lastByName = new Map<string, TjFlow>();
  for (const f of flows) lastByName.set(f.flow_name, f);
  return new Set(flows.filter((f) => lastByName.get(f.flow_name) !== f).map((f) => f.id));
}

/// Effective path: for each stage, the latest attempt determines the outcome;
/// superseded attempts are collapsed into an expandable marker.
function groupStages(stages: TjStage[]): StageGroup[] {
  const order: StageGroup[] = [];
  const byName = new Map<string, StageGroup>();
  for (const s of stages) { // arrival order = execution order
    const g = byName.get(s.stage_name);
    if (!g) {
      const group = { latest: s, earlier: [] as TjStage[] };
      byName.set(s.stage_name, group);
      order.push(group);
    } else {
      g.earlier.push(g.latest);
      g.latest = s;
    }
  }
  return order;
}
type Selection = { kind: 'run' } | { kind: 'flow'; name: string } | { kind: 'stage'; id: string } | { kind: 'gate' };

function scopeLabel(sel: Selection): string {
  if (sel.kind === 'run') return 'Whole run';
  if (sel.kind === 'flow') return `Flow · ${sel.name}`;
  if (sel.kind === 'gate') return 'Approval gate';
  return 'Stage';
}

function FlowTree({ flows, selection, onSelect, gate, showAll }: { flows: TjFlow[]; selection: Selection; onSelect: (s: Selection) => void; gate: boolean; showAll: boolean }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const stale = supersededFlowIds(flows);
  const toggleExpand = (id: string) => setExpanded((prev) => {
    const next = new Set(prev);
    if (next.has(id)) next.delete(id); else next.add(id);
    return next;
  });
  // flows arrive in execution order (oldest first) — render as-is.
  return (
    <>
      {gate && (
        <button
          className="rowbtn"
          style={{ padding: '3px 6px', gap: 7, background: selection.kind === 'gate' ? 'var(--sunk)' : undefined, borderRadius: 'var(--r)' }}
          aria-current={selection.kind === 'gate'}
          onClick={() => onSelect({ kind: 'gate' })}
        >
          <Dot status="waiting_for_gate" />
          <span style={{ fontWeight: 600, fontSize: 13, flex: 1, textAlign: 'left' }}>Approval gate</span>
          <span className="chip c-wait">decision</span>
        </button>
      )}
      {flows.map((f) => {
        const flowSel = selection.kind === 'flow' && selection.name === f.flow_name;
        const isStale = stale.has(f.id);
        return (
          <div key={f.id} style={{ marginBottom: 10, opacity: isStale ? 0.6 : 1 }}>
            <button
              className="rowbtn"
              style={isStale ? { padding: '4px 6px', gap: 6, background: flowSel ? 'var(--sunk)' : undefined, flexDirection: 'column', alignItems: 'stretch' } : { padding: '3px 6px', gap: 7, background: flowSel ? 'var(--sunk)' : undefined }}
              aria-current={flowSel}
              onClick={() => onSelect({ kind: 'flow', name: f.flow_name })}
            >
              <span style={{ display: 'flex', gap: 7, alignItems: 'center', minWidth: 0 }}>
                <Dot status={f.status} />
                <span style={{ fontWeight: 600, fontSize: 13, flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textAlign: 'left' }}>{f.flow_name}</span>
                {isStale && <span className="chip c-idle" style={{ flexShrink: 0 }}>superseded</span>}
              </span>
              {isStale
                ? <span className="tstamp" style={{ marginLeft: 13 }}>{clockTime(f.started_at)} · {duration(f.started_at, f.completed_at)}</span>
                : <>
                    <span className="tstamp" style={{ width: 64, textAlign: 'right' }}>{clockTime(f.started_at)}</span>
                    <span className="tstamp" style={{ width: 44, textAlign: 'right' }}>{duration(f.started_at, f.completed_at)}</span>
                  </>}
            </button>
            <StageRows
              groups={groupStages(f.stages || [])}
              selection={selection}
              onSelect={onSelect}
              showAll={showAll}
              expanded={expanded}
              toggleExpand={toggleExpand}
            />
          </div>
        );
      })}
    </>
  );
}

function StageRows({ groups, selection, onSelect, showAll, expanded, toggleExpand }: {
  groups: StageGroup[]; selection: Selection; onSelect: (s: Selection) => void;
  showAll: boolean; expanded: Set<string>; toggleExpand: (id: string) => void;
}) {
  return (
    <div style={{ margin: '4px 0 0 6px', borderLeft: '2px solid var(--rule)', paddingLeft: 10, display: 'flex', flexDirection: 'column', gap: 3 }}>
      {groups.map((g) => {
        const s = g.latest;
        const stageSel = selection.kind === 'stage' && selection.id === s.id;
        const open = showAll || expanded.has(s.id);
        const dur = (x: TjStage) => (
          <>
            <span className="tstamp" style={{ width: 64, textAlign: 'right' }}>{clockTime(x.started_at)}</span>
            <span className="tstamp" style={{ width: 44, textAlign: 'right' }}>{duration(x.started_at, x.completed_at)}</span>
          </>
        );
        return (
          <div key={s.id} style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
            <button className="rowbtn" style={{ padding: '3px 6px', gap: 7, background: stageSel ? 'var(--sunk)' : undefined }} aria-current={stageSel} onClick={() => onSelect({ kind: 'stage', id: s.id })}>
              <Dot status={s.status} />
              <span style={{ flex: 1, textAlign: 'left', minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {s.stage_name}
                {s.iteration > 0 && <span className="tiny dim"> · attempt {s.iteration + 1}</span>}
              </span>
              {dur(s)}
            </button>
            {g.earlier.length > 0 && !showAll && (
              <button className="tbtn" style={{ alignSelf: 'flex-start', marginLeft: 17 }} onClick={() => toggleExpand(s.id)}>
                {open ? 'Hide' : 'Show'} {g.earlier.length} earlier attempt{g.earlier.length === 1 ? '' : 's'}
              </button>
            )}
            {(open ? g.earlier : []).map((x) => (
              <button key={x.id} className="rowbtn" style={{ padding: '3px 6px', gap: 7, opacity: 0.65, background: selection.kind === 'stage' && selection.id === x.id ? 'var(--sunk)' : undefined }} onClick={() => onSelect({ kind: 'stage', id: x.id })}>
                <Dot status={x.status} />
                <span style={{ flex: 1, textAlign: 'left', minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {x.stage_name}
                  <span className="tiny dim"> · attempt {x.iteration + 1} (superseded)</span>
                </span>
                {dur(x)}
              </button>
            ))}
          </div>
        );
      })}
    </div>
  );
}

export function RunDetail({ nav, runId }: { nav: Nav; runId: string }) {
  const tree = useRunTree(runId);
  const decisions = useQuery({ queryKey: ['decisions', runId], queryFn: () => api.decisions(runId) });
  const [tab, setTab] = useState<CenterTab>('logs');
  const [selection, setSelection] = useState<Selection>({ kind: 'run' });
  const [showAll, setShowAll] = useState(() => localStorage.getItem('tj-show-attempts') === '1');
  const toggleShowAll = () => setShowAll((v) => { localStorage.setItem('tj-show-attempts', v ? '0' : '1'); return !v; });

  useEffect(() => {
    setSelection({ kind: 'run' });
    setTab('logs');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runId]);

  // Stage selection opens its conversation; run/flow selection falls back to logs.
  const select = (sel: Selection) => {
    setSelection(sel);
    if (sel.kind === 'stage') setTab('chat');
    else if (sel.kind !== 'gate') setTab('logs');
  };

  if (tree.isLoading) return <div className="card pad tiny dim">Loading run…</div>;
  if (tree.error) return <div className="card pad tiny dim" style={{ color: 'var(--fail)' }}>Failed to load run: {(tree.error as Error).message}</div>;

  const { run, flows } = tree.data!;
  // A resume creates a NEW row for the same flow name — only the latest
  // execution per name is real; older rows are abandoned leftovers.
  // A resume creates a NEW row for the same flow name — only the latest
  // execution per name is real; older rows are abandoned leftovers.
  const byName = new Map<string, TjFlow>();
  for (const f of flows) byName.set(f.flow_name, f); // arrival order = execution order, last wins
  const liveFlows = [...byName.values()];
  const active = !['completed', 'failed', 'cancelled'].includes(run.status);
  const dec: GateDecision[] = decisions.data || [];
  const selectedStage = selection.kind === 'stage'
    ? liveFlows.flatMap((f) => f.stages.map((s) => ({ s, f }))).find((x) => x.s.id === selection.id)
    : undefined;

  return (
    <>
      <div className="crumb">
        <button onClick={() => nav.setView('runs')}>← Runs</button>
        <span className="sep">/</span>
        <span className="here">{run.description || run.id}</span>
        <span style={{ marginLeft: 'auto' }} />
        <span className="tiny dim">{relTime(run.created_at)} · </span>
        <StatusChip status={run.status} />
        <span style={{ display: 'inline-flex', gap: 8, marginLeft: 10, alignItems: 'center' }}>
          <RetryRunButton run={run} />
          {!TERMINAL.includes(run.status) && <CancelRunButton run={run} />}
          {TERMINAL.includes(run.status) && (
            <DeleteRunButton run={run} onDeleted={() => { nav.setView('runs'); }} />
          )}
        </span>
      </div>

      <div className="three" style={{ gridTemplateColumns: '280px minmax(0,1fr)' }}>
        <div className="card pad">
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 6 }}>
            <p className="label" style={{ margin: 0 }}>Scope — click to filter</p>
            <button className={`tbtn${showAll ? ' on' : ''}`} aria-pressed={showAll} title="Show superseded attempts" onClick={toggleShowAll}>All attempts</button>
          </div>
          <button className="rowbtn" style={{ padding: '4px 6px', marginBottom: 6, background: selection.kind === 'run' ? 'var(--sunk)' : undefined }} aria-current={selection.kind === 'run'} onClick={() => setSelection({ kind: 'run' })}>
            <Dot status={run.status} />
            <span style={{ fontWeight: 600, fontSize: 13, flex: 1, textAlign: 'left' }}>Whole run</span>
          </button>
          <FlowTree flows={showAll ? flows : liveFlows} selection={selection} onSelect={select} gate={run.status === 'waiting_for_gate'} showAll={showAll} />
          <div style={{ borderTop: '1px solid var(--rule)', margin: '10px 0 0', paddingTop: 10 }}>
            <p className="label">Decisions</p>
            {dec.length ? dec.map((d) => (
              <div key={d.id} className="evt">
                <time>{d.created_at.slice(11, 16)}</time>
                <span className={`k ${d.approved ? 'k-ok' : 'k-fail'}`} />
                <span style={{ flex: 1, minWidth: 0 }}>
                  {d.approved ? 'approved' : 'rejected'} by {d.actor_name}
                </span>
              </div>
            )) : <p className="tiny dim">None yet.</p>}
            <p className="label" style={{ marginTop: 12 }}>Catalog</p>
            <p className="tiny" style={{ margin: 0 }}>
              {run.catalog_id || run.workflow_id || run.profile
                ? <><span className="chip c-wait">{run.catalog_id || run.workflow_id || run.profile}</span> <span className="dim">behavior comes from the frozen contract</span></>
                : <span className="dim">—</span>}
            </p>
            <p className="label" style={{ marginTop: 12 }}>Repository</p>
            <p className="tiny dim mono" style={{ margin: 0, wordBreak: 'break-all' }}>{run.repo_url || '— (artifact workspace only)'}</p>
            {run.pr_url && <a className="tiny" href={run.pr_url} target="_blank" rel="noreferrer">Pull request #{run.pr_number}</a>}
            <p className="label" style={{ marginTop: 12 }}>Publication</p>
            <PublicationPanel runId={run.id} />
            <p className="label" style={{ marginTop: 12 }}>Delivery</p>
            <DeliveryPanel runId={run.id} />
            <p className="label" style={{ marginTop: 12 }}>Budget</p>
            <BudgetPanel runId={run.id} />
            <p className="label" style={{ marginTop: 12 }}>Diagnostics</p>
            <DiagnosticsPanel runId={run.id} />
            <p className="label" style={{ marginTop: 12 }}>Run id</p>
            <p className="tiny dim mono" style={{ margin: 0, wordBreak: 'break-all' }}>{run.id}</p>
          </div>
        </div>

        <div className="card pad" style={{ display: 'flex', flexDirection: 'column' }}>
          <div style={{ display: 'flex', gap: 6, marginBottom: 10, alignItems: 'center', flexWrap: 'wrap', flexShrink: 0 }}>
            {((selection.kind === 'stage' ? ['chat', 'logs', 'artifacts', 'files', 'plan'] : ['logs', 'artifacts', 'files', 'plan']) as CenterTab[]).map((t) => (
              <button key={t} className="fchip" aria-pressed={tab === t} onClick={() => setTab(t)}>
                {t === 'chat' ? 'Chat' : t === 'logs' ? 'Log' : t[0].toUpperCase() + t.slice(1)}
              </button>
            ))}
            {tab === 'logs' && active && <span className="chip c-wait">live</span>}
            <span className="tiny dim" style={{ marginLeft: 'auto' }}>scope: {scopeLabel(selection)}</span>
          </div>

          {run.status === 'interrupted' && (
            <div className="banner" style={{ marginBottom: 10, flexShrink: 0, display: 'block' }}>
              <span>Run interrupted by a restart. Accepted answers and artifacts are preserved; start an explicit retry from the header. No execution is replayed automatically.</span>
              <InterruptedQuestions runId={run.id} />
            </div>
          )}
          {run.status === 'waiting_for_gate' && run.gate_state === 'task_selection_required' && (
            <div className="banner" style={{ marginBottom: 10, flexShrink: 0, display: 'block' }}>
              <span>
                Plan approved. The selected flow is <strong>not</strong> running yet: select exactly one ready task in the Plan tab.
                No model or repository action starts without that durable selection.
              </span>
              <button className="btn btn-sm btn-go" style={{ marginTop: 6 }} onClick={() => setTab('plan')}>Select a ready task →</button>
            </div>
          )}
          {run.status === 'waiting_for_gate' && run.gate_state !== 'task_selection_required' && selection.kind === 'gate' && (
            <GateReview run={run} flows={showAll ? flows : liveFlows} decisions={dec} showAll={showAll} />
          )}
          {run.status === 'waiting_for_gate' && run.gate_state !== 'task_selection_required' && selection.kind !== 'gate' && (
            <div className="banner" style={{ marginBottom: 10, flexShrink: 0 }}>
              <span>Pipeline paused at the approval gate.</span>
              <button className="btn btn-sm btn-go" onClick={() => select({ kind: 'gate' })}>Review & decide →</button>
            </div>
          )}
          {run.status === 'running' && selection.kind !== 'gate' && selection.kind !== 'stage' && <WaitingBanner flows={liveFlows} onSelect={(id) => select({ kind: 'stage', id })} />}
          {selection.kind !== 'gate' && selection.kind === 'stage' && tab === 'chat' && selectedStage && (
            <div style={{ height: 'calc(100vh - 240px)', minHeight: 320, display: 'flex', flexDirection: 'column', flexShrink: 0 }}>
                <StageChat runId={run.id} stageId={selectedStage.s.id} stageIteration={selectedStage.s.iteration} stageName={selectedStage.s.stage_name} stageStatus={selectedStage.s.status} />
            </div>
          )}
          {selection.kind !== 'gate' && tab === 'logs' && (
            <div>
              {selectedStage && (
                <div style={{ marginBottom: 10, padding: '8px 10px', background: 'var(--sunk)', borderRadius: 'var(--r)', flexShrink: 0 }}>
                  <div className="stepmeta">
                    <StatusChip status={selectedStage.s.status} />
                    <span className="mono" style={{ fontWeight: 600 }}>{selectedStage.s.stage_name}</span>
                    <span className="tiny dim">{selectedStage.f.flow_name}{duration(selectedStage.s.started_at, selectedStage.s.completed_at) ? ` · took ${duration(selectedStage.s.started_at, selectedStage.s.completed_at)}` : ''}</span>
                  </div>
                  {selectedStage.s.error && (
                    <div className="term" style={{ borderColor: 'var(--fail-line)', color: 'var(--fail)', marginTop: 6, maxHeight: 'none', overflow: 'visible' }}>{selectedStage.s.error}</div>
                  )}
                </div>
              )}
              <ActivityPanel runId={run.id} active={active} selection={selection} flows={flows} />

            </div>
          )}
          {selection.kind !== 'gate' && tab === 'artifacts' && <ArtifactsPanel runId={run.id} selection={selection} flows={liveFlows} />}
          {selection.kind !== 'gate' && tab === 'files' && <FilesPanel runId={run.id} />}
          {selection.kind !== 'gate' && tab === 'plan' && <PlanReview runId={run.id} />}
        </div>
      </div>
    </>
  );
}
