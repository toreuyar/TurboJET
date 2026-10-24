import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import type { PlanBundle, PlanRevision, PlanTask, TaskReadiness } from '../types';

function diffLines(before: string, after: string) {
  const a = before.split('\n');
  const b = after.split('\n');
  const table = Array.from({ length: a.length + 1 }, () => new Array(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      table[i][j] = a[i] === b[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }
  const out: { kind: 'same' | 'added' | 'removed'; text: string }[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) { out.push({ kind: 'same', text: a[i] }); i += 1; j += 1; }
    else if (table[i + 1][j] >= table[i][j + 1]) { out.push({ kind: 'removed', text: a[i] }); i += 1; }
    else { out.push({ kind: 'added', text: b[j] }); j += 1; }
  }
  while (i < a.length) { out.push({ kind: 'removed', text: a[i] }); i += 1; }
  while (j < b.length) { out.push({ kind: 'added', text: b[j] }); j += 1; }
  return out;
}

function taskRows(revision: PlanRevision): PlanTask[] {
  return (revision.content?.tasks ?? []) as PlanTask[];
}

function readinessFor(bundle: PlanBundle | undefined, taskId: string): TaskReadiness | undefined {
  return bundle?.readiness?.find((item) => item.task_id === taskId);
}

export function PlanReview({ runId, hideWhenMissing = false }: { runId: string; hideWhenMissing?: boolean }) {
  const qc = useQueryClient();
  const planQ = useQuery<PlanBundle>({
    queryKey: ['plan', runId],
    queryFn: () => api.plan(runId),
    refetchInterval: 5000,
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [changeRequest, setChangeRequest] = useState('');
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const bundle = planQ.data;
  const latest = bundle?.latest ?? null;
  const revisions = bundle?.revisions ?? [];
  const selected = revisions.find((revision) => revision.id === selectedId) ?? latest;
  const previous = selected
    ? revisions.filter((revision) => revision.revision < selected.revision).slice(-1)[0] ?? null
    : null;
  const diff = useMemo(
    () => (previous && selected
      ? diffLines(JSON.stringify(previous.content, null, 2), JSON.stringify(selected.content, null, 2)).filter((line) => line.kind !== 'same')
      : []),
    [previous, selected],
  );

  const save = useMutation({
    mutationFn: () => {
      if (!selected) throw new Error('There is no plan revision to revise.');
      return api.savePlanRevision(runId, JSON.parse(draft) as Record<string, unknown>, {
        planId: selected.plan_id,
        changeRequest: changeRequest.trim() || 'Reviewer plan revision',
      });
    },
    onSuccess: (revision) => {
      setActionError(null);
      setNotice(`Plan revision ${revision.revision} saved with hash ${revision.content_hash.slice(0, 12)}…; any earlier plan approval is stale.`);
      setEditing(false);
      setSelectedId(revision.id);
      qc.invalidateQueries({ queryKey: ['plan', runId] });
      qc.invalidateQueries({ queryKey: ['tree', runId] });
      qc.invalidateQueries({ queryKey: ['decisions', runId] });
    },
    onError: (error) => { setNotice(null); setActionError((error as Error).message); },
  });

  const download = useMutation({
    mutationFn: (format: 'markdown' | 'json') => api.downloadPlan(runId, format, selected?.id),
    onError: (error) => { setNotice(null); setActionError(`Download failed: ${(error as Error).message}`); },
  });

  const selectTask = useMutation({
    mutationFn: (task: PlanTask) => {
      if (!selected) throw new Error('There is no approved plan revision to select from.');
      return api.selectPlanTask(runId, {
        plan_revision_id: selected.id,
        plan_content_hash: selected.content_hash,
        task_id: task.id,
      });
    },
    onSuccess: (result) => {
      setActionError(null);
      setNotice(`Task ${result.selection.task_id} selected for the next flow from plan ${result.selection.plan_revision_id.slice(0, 12)}… (${result.replayed ? 'replayed' : 'recorded'}); criteria and provenance are retained in the handoff payload.`);
      qc.invalidateQueries({ queryKey: ['plan', runId] });
      qc.invalidateQueries({ queryKey: ['events', runId] });
      qc.invalidateQueries({ queryKey: ['tree', runId] });
      qc.invalidateQueries({ queryKey: ['runs'] });
      qc.invalidateQueries({ queryKey: ['publication', runId] });
    },
    onError: (error) => { setNotice(null); setActionError((error as Error).message); },
  });

  if (planQ.isLoading) return <p className="tiny dim">Loading structured task plan…</p>;
  if (planQ.error) {
    if (hideWhenMissing) return null;
    return <p className="tiny" role="alert" style={{ color: 'var(--fail)' }}>Unable to load plan: {(planQ.error as Error).message}</p>;
  }
  if (!latest || !selected) {
    if (hideWhenMissing) return null;
    return (
      <div className="panel" style={{ marginBottom: 12 }}>
        <p className="label" style={{ marginTop: 0 }}>Structured task plan</p>
        <p className="tiny dim" style={{ margin: 0 }}>
          No plan revision exists yet. Planning must produce a deterministically validated plan bound to the exact approved packet before this gate can be approved.
        </p>
      </div>
    );
  }

  const tasks = taskRows(selected);
  const validation = selected.validation ?? { valid: true, task_count: tasks.length, requires_packet_revision: false, blocked_tasks: [], open_decisions: [] };
  const scopeExpansions = selected.content?.scope_expansion_requests ?? [];
  const approved = bundle?.approved ?? null;
  const canSelect = Boolean(approved && approved.id === selected.id);
  const selectionByTask = new Map((bundle?.selections ?? []).map((item) => [item.task_id, item]));

  return (
    <section aria-label="Structured task plan review" style={{ marginBottom: 14 }}>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 8 }}>
        <h2 style={{ margin: 0 }}>Structured task plan</h2>
        <span className="chip c-wait">experimental</span>
        <span className={`chip ${validation.requires_packet_revision ? 'c-wait' : 'c-done'}`}>
          {validation.requires_packet_revision ? 'needs packet revision' : 'validated'}
        </span>
        <span className="tiny dim mono">{selected.plan_id} · r{selected.revision} · {selected.content_hash.slice(0, 16)}…</span>
      </div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 8 }}>
        <label className="tiny dim" htmlFor="plan-revision-select">Revision</label>
        <select
          id="plan-revision-select"
          value={selected.id}
          onChange={(event) => { setSelectedId(event.target.value); setEditing(false); }}
        >
          {revisions.map((revision) => (
            <option key={revision.id} value={revision.id}>
              r{revision.revision} · {revision.content_hash.slice(0, 10)} · packet r{revision.packet_revision}
            </option>
          ))}
        </select>
        <button className="btn btn-sm" type="button" disabled={download.isPending} onClick={() => download.mutate('markdown')}>Download Markdown</button>
        <button className="btn btn-sm" type="button" disabled={download.isPending} onClick={() => download.mutate('json')}>Download JSON</button>
        <button
          className="btn btn-sm"
          type="button"
          onClick={() => {
            setDraft(JSON.stringify(selected.content, null, 2));
            setEditing((value) => !value);
            setActionError(null);
          }}
        >{editing ? 'Close editor' : 'Revise plan'}</button>
      </div>
      {notice && <p className="tiny" role="status" style={{ color: 'var(--done)' }}>{notice}</p>}
      {actionError && <p className="tiny" role="alert" style={{ color: 'var(--fail)' }}>{actionError}</p>}

      <div className="panel" style={{ marginBottom: 10 }}>
        <h3 style={{ margin: '0 0 8px' }}>{selected.content?.title ?? 'Untitled task plan'}</h3>
        <p style={{ margin: '0 0 4px', whiteSpace: 'pre-wrap' }}>{selected.content?.summary ?? ''}</p>
        <p className="tiny dim" style={{ margin: 0 }}>
          Source packet: <span className="mono">{selected.packet_id}</span> r{selected.packet_revision} ·{' '}
          <span className="mono">{selected.packet_content_hash.slice(0, 16)}…</span> · bound revision <span className="mono">{selected.packet_revision_id.slice(0, 12)}…</span>
        </p>
        <p className="tiny dim" style={{ margin: '4px 0 0' }}>
          Deterministic validation precedes the human gate: {validation.task_count} task(s), {validation.blocked_tasks.length} blocked, {validation.open_decisions.length} open decision(s).
          Planning is read-only — no repository writes, commits, pushes or pull requests occur.
        </p>
      </div>

      {validation.requires_packet_revision && scopeExpansions.length > 0 && (
        <div className="panel" style={{ marginBottom: 10, borderColor: 'var(--wait-line, var(--rule))' }}>
          <p className="label" style={{ marginTop: 0, color: 'var(--wait)' }}>Scope expansion requires a new packet revision</p>
          {scopeExpansions.map((request) => (
            <p key={request.id} className="tiny" style={{ margin: '4px 0' }}>
              <strong>{request.id}</strong> — {request.description} · requested by {request.requested_by}. {request.rationale}
              {' '}This is not a task until the requirements packet is revised and re-approved.
            </p>
          ))}
        </div>
      )}

      <div className="panel" style={{ marginBottom: 10 }}>
        <p className="label" style={{ marginTop: 0 }}>Tasks, dependencies and evidence</p>
        {tasks.map((task) => {
          const readiness = readinessFor(bundle, task.id);
          const validationBlock = validation.blocked_tasks.find((block) => block.task_id === task.id);
          const reasons = readiness?.blocked_reasons?.length ? readiness.blocked_reasons : (validationBlock?.reasons ?? []);
          const ready = canSelect && (readiness?.ready ?? reasons.length === 0);
          const selection = selectionByTask.get(task.id);
          return (
            <div key={task.id} style={{ borderTop: '1px solid var(--rule)', paddingTop: 8, marginTop: 8 }}>
              <div style={{ display: 'flex', gap: 7, alignItems: 'center', flexWrap: 'wrap' }}>
                <strong>{task.id} — {task.title}</strong>
                <span className={`chip ${ready ? 'c-done' : 'c-wait'}`}>{ready ? 'ready' : reasons.length ? 'blocked' : 'planned'}</span>
                <span className="tiny dim">{task.estimate.size} · uncertainty {task.estimate.uncertainty}</span>
                {selection && <span className="chip c-done">selected {selection.selected_by}</span>}
              </div>
              <p className="tiny" style={{ margin: '3px 0' }}>{task.objective}</p>
              <p className="tiny dim" style={{ margin: '2px 0' }}>
                Requirements: {task.requirement_refs?.length ? task.requirement_refs.join(', ') : '—'}
                {task.enabling_rationale ? ` · enabling rationale: ${task.enabling_rationale}` : ''}
              </p>
              <p className="tiny dim" style={{ margin: '2px 0' }}>
                Dependencies: {task.dependencies?.length ? task.dependencies.join(', ') : 'none'} · Components: {task.components?.join(', ')}
              </p>
              <ul className="tiny" style={{ margin: '2px 0 0 16px', padding: 0 }}>
                {(task.acceptance_criteria ?? []).map((criterion, index) => <li key={`${task.id}-c-${index}`}>Acceptance: {criterion}</li>)}
                {(task.verification ?? []).map((step, index) => <li key={`${task.id}-v-${index}`}>Verification: {step}</li>)}
              </ul>
              {(task.risks ?? []).length > 0 && (
                <p className="tiny dim" style={{ margin: '3px 0 0' }}>
                  Risks: {(task.risks ?? []).map((risk) => `${risk.id} ${risk.statement} (${risk.severity}; ${risk.mitigation})`).join(' · ')}
                </p>
              )}
              {reasons.length > 0 && (
                <p className="tiny" role="status" style={{ margin: '4px 0 0', color: 'var(--wait)' }}>
                  Blocked: {reasons.join('; ')}
                </p>
              )}
              <div style={{ marginTop: 5 }}>
                <button
                  className="btn btn-sm"
                  type="button"
                  disabled={!canSelect || !ready || selectTask.isPending}
                  title={!canSelect ? 'Approve the exact plan revision at the planning gate before selecting a task' : !ready ? 'Blocked tasks cannot be selected' : 'Record the task handoff for this ready task'}
                  onClick={() => selectTask.mutate(task)}
                >Select task</button>
              </div>
            </div>
          );
        })}
        {!tasks.length && <p className="tiny dim">No tasks recorded.</p>}
      </div>

      <details className="panel" style={{ marginBottom: 10 }}>
        <summary className="rowbtn" style={{ padding: 0 }}><span className="acc">▸</span> Changes since previous plan revision</summary>
        {!previous && <p className="tiny dim">This is the first plan revision.</p>}
        {previous && diff.length === 0 && <p className="tiny dim">No textual differences from r{previous.revision}.</p>}
        {diff.length > 0 && (
          <pre className="term" aria-label={`Plan diff from revision ${previous?.revision}`} style={{ maxHeight: 260 }}>
            {diff.map((line, index) => (
              <div key={index} style={{ color: line.kind === 'added' ? 'var(--done)' : 'var(--fail)' }}>
                {line.kind === 'added' ? '+ ' : '- '}{line.text}
              </div>
            ))}
          </pre>
        )}
      </details>

      {editing && (
        <div className="panel" style={{ marginBottom: 10 }}>
          <label className="label" htmlFor="plan-json">Plan JSON (task-plan schema v1)</label>
          <textarea
            id="plan-json"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            style={{ width: '100%', minHeight: 280, fontFamily: 'monospace', fontSize: 12 }}
          />
          <div className="field" style={{ marginTop: 8 }}>
            <label htmlFor="plan-change-request">Reason for the revision</label>
            <input id="plan-change-request" value={changeRequest} onChange={(event) => setChangeRequest(event.target.value)} placeholder="e.g. split the notification task" />
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button className="btn btn-go" type="button" disabled={save.isPending} onClick={() => { setActionError(null); save.mutate(); }}>Save plan revision</button>
            <button className="btn" type="button" onClick={() => setEditing(false)}>Cancel</button>
          </div>
          <p className="tiny dim" style={{ margin: '6px 0 0' }}>
            Saving creates a new immutable plan revision bound to the same approved packet. Any approval prepared for the previous plan becomes stale.
          </p>
        </div>
      )}

      <div className="panel">
        <p className="label" style={{ marginTop: 0 }}>Provenance, approval and history</p>
        <p className="tiny dim" style={{ margin: 0 }}>
          Created by {selected.created_by} at {selected.created_at}
          {selected.source_artifact_id ? ` · artifact ${selected.source_artifact_id}` : ''}
          {selected.parent_revision_id ? ` · parent ${selected.parent_revision_id}` : ''}
          {selected.change_request ? ` · change request: ${selected.change_request}` : ''}
        </p>
        {(bundle?.reviews ?? []).map((review) => (
          <p key={review.id} className="tiny" style={{ margin: '5px 0 0' }}>
            <strong>{review.outcome}</strong> by {review.actor_name} at {review.created_at}
            {review.reason ? ` — ${review.reason}` : ''} · plan r{review.revision} · <span className="mono">{review.content_hash.slice(0, 12)}…</span>
            {' '}· packet <span className="mono">{review.packet_content_hash.slice(0, 12)}…</span>
          </p>
        ))}
        {!(bundle?.reviews ?? []).length && <p className="tiny dim" style={{ margin: '5px 0 0' }}>No plan gate reviews recorded.</p>}
      </div>
      <p className="tiny dim" style={{ margin: '6px 0 0' }}>
        Task planning is <strong>experimental</strong>. Exported plan JSON/Markdown is portable and separate from stage transcript exports.
      </p>
    </section>
  );
}
