import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api';
import type { PacketBundle, PacketRevision } from '../types';

function SourceList({ refs, packet }: { refs?: string[]; packet: PacketRevision }) {
  if (!refs?.length) return <span className="tiny dim">No sources cited.</span>;
  const sources = (packet.content?.sources ?? []) as { id: string; kind: string; reference: string; excerpt: string }[];
  return (
    <ul className="tiny" style={{ margin: '4px 0 0 16px', padding: 0 }}>
      {refs.map((ref) => {
        const source = sources.find((item) => item.id === ref);
        return (
          <li key={ref}>
            <span className="mono">{ref}</span>
            {source ? <> — {source.reference}: {source.excerpt}</> : <span style={{ color: 'var(--fail)' }}> — missing source</span>}
          </li>
        );
      })}
    </ul>
  );
}

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

function requirementRows(packet: PacketRevision) {
  const requirements = (packet.content?.requirements ?? []) as Record<string, unknown>[];
  return requirements.map((requirement) => ({
    id: String(requirement.id ?? ''),
    title: String(requirement.title ?? ''),
    statement: String(requirement.statement ?? ''),
    status: String(requirement.status ?? ''),
    priority: String(requirement.priority ?? ''),
    criteria: (requirement.acceptance_criteria as string[] | undefined) ?? [],
    sources: (requirement.source_refs as string[] | undefined) ?? [],
  }));
}

export function PacketReview({ runId, readOnly = false, onChanged }: {
  runId: string;
  readOnly?: boolean;
  onChanged?: () => void;
}) {
  const qc = useQueryClient();
  const packetQ = useQuery<PacketBundle>({
    queryKey: ['packet', runId],
    queryFn: () => api.packet(runId),
    refetchInterval: 5000,
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [changeRequest, setChangeRequest] = useState('');
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const bundle = packetQ.data;
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
      if (!selected) throw new Error('There is no packet revision to revise.');
      return api.savePacketRevision(runId, JSON.parse(draft) as Record<string, unknown>, {
        packetId: selected.packet_id,
        changeRequest: changeRequest.trim() || 'Reviewer revision',
      });
    },
    onSuccess: (revision) => {
      setActionError(null);
      setNotice(`Revision ${revision.revision} saved with hash ${revision.content_hash.slice(0, 12)}…; the previous approval cannot carry over.`);
      setEditing(false);
      setSelectedId(revision.id);
      qc.invalidateQueries({ queryKey: ['packet', runId] });
      qc.invalidateQueries({ queryKey: ['tree', runId] });
      qc.invalidateQueries({ queryKey: ['decisions', runId] });
      onChanged?.();
    },
    onError: (error) => { setNotice(null); setActionError((error as Error).message); },
  });

  const download = useMutation({
    mutationFn: (format: 'markdown' | 'json') => api.downloadPacket(runId, format, selected?.id),
    onError: (error) => { setNotice(null); setActionError(`Download failed: ${(error as Error).message}`); },
  });

  if (packetQ.isLoading) return <p className="tiny dim">Loading requirements packet…</p>;
  if (packetQ.error) return <p className="tiny" role="alert" style={{ color: 'var(--fail)' }}>Unable to load packet: {(packetQ.error as Error).message}</p>;
  if (!latest || !selected) {
    return (
      <div className="panel" style={{ marginBottom: 12 }}>
        <p className="label" style={{ marginTop: 0 }}>Requirements packet</p>
        <p className="tiny dim" style={{ margin: 0 }}>No packet revision exists yet. The Discovery generation stage must produce one before this gate can be approved.</p>
      </div>
    );
  }

  const requirements = requirementRows(selected);
  const unknowns = (selected.content?.unknowns ?? []) as Record<string, unknown>[];
  const decisions = (selected.content?.decisions ?? []) as Record<string, unknown>[];
  const contradictions = (selected.content?.contradictions ?? []) as Record<string, unknown>[];
  const evidence = (selected.content?.evidence ?? []) as Record<string, unknown>[];
  const completenessClass = selected.completeness === 'complete' ? 'c-done' : selected.completeness === 'contradictory' ? 'c-fail' : 'c-wait';

  return (
    <section aria-label="Requirements packet review" style={{ marginBottom: 14 }}>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 8 }}>
        <h2 style={{ margin: 0 }}>Requirements packet</h2>
        <span className={`chip ${completenessClass}`}>{selected.completeness}</span>
        <span className="tiny dim mono">{selected.packet_id} · r{selected.revision} · {selected.content_hash.slice(0, 16)}…</span>
      </div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 8 }}>
        <label className="tiny dim" htmlFor="packet-revision-select">Revision</label>
        <select
          id="packet-revision-select"
          value={selected.id}
          onChange={(event) => { setSelectedId(event.target.value); setEditing(false); }}
        >
          {revisions.map((revision) => (
            <option key={revision.id} value={revision.id}>
              r{revision.revision} · {revision.completeness} · {revision.content_hash.slice(0, 10)}
            </option>
          ))}
        </select>
        <button className="btn btn-sm" type="button" disabled={download.isPending} onClick={() => download.mutate('markdown')}>Download Markdown</button>
        <button className="btn btn-sm" type="button" disabled={download.isPending} onClick={() => download.mutate('json')}>Download JSON</button>
        {!readOnly && (
          <button
            className="btn btn-sm"
            type="button"
            onClick={() => {
              setDraft(JSON.stringify(selected.content, null, 2));
              setEditing((value) => !value);
              setActionError(null);
            }}
          >
            {editing ? 'Close editor' : 'Revise packet'}
          </button>
        )}
      </div>
      {notice && <p className="tiny" role="status" style={{ color: 'var(--done)' }}>{notice}</p>}
      {actionError && <p className="tiny" role="alert" style={{ color: 'var(--fail)' }}>{actionError}</p>}

      <div className="panel" style={{ marginBottom: 10 }}>
        <h3 style={{ margin: '0 0 8px' }}>{String(selected.content?.title ?? 'Untitled requirements packet')}</h3>
        <p className="label">Problem</p>
        <p style={{ margin: '0 0 4px', whiteSpace: 'pre-wrap' }}>{String(selected.content?.problem?.statement ?? '')}</p>
        <SourceList packet={selected} refs={selected.content?.problem?.source_refs as string[] | undefined} />
      </div>

      <div className="panel" style={{ marginBottom: 10 }}>
        <p className="label" style={{ marginTop: 0 }}>Requirements and acceptance criteria</p>
        {requirements.length === 0 && <p className="tiny dim">No requirements recorded.</p>}
        {requirements.map((requirement) => (
          <div key={requirement.id} style={{ borderTop: '1px solid var(--rule)', paddingTop: 7, marginTop: 7 }}>
            <strong>{requirement.id} — {requirement.title}</strong>{' '}
            <span className={`chip ${requirement.status === 'confirmed' ? 'c-done' : 'c-wait'}`}>{requirement.status}</span>{' '}
            <span className="tiny dim">{requirement.priority}</span>
            <p className="tiny" style={{ margin: '3px 0' }}>{requirement.statement}</p>
            <ul className="tiny" style={{ margin: '2px 0 0 16px', padding: 0 }}>
              {requirement.criteria.map((criterion, index) => <li key={`${requirement.id}-${index}`}>{criterion}</li>)}
            </ul>
            <SourceList packet={selected} refs={requirement.sources} />
          </div>
        ))}
      </div>

      <details className="panel" style={{ marginBottom: 10 }}>
        <summary className="rowbtn" style={{ padding: 0 }}><span className="acc">▸</span> Evidence ({evidence.length})</summary>
        {evidence.length === 0 && <p className="tiny dim">No evidence claims.</p>}
        {evidence.map((item) => (
          <div key={String(item.id)} style={{ marginTop: 6 }}>
            <span className="mono tiny">{String(item.id)}</span> — {String(item.claim)}
            <SourceList packet={selected} refs={item.source_refs as string[] | undefined} />
          </div>
        ))}
      </details>

      <details className="panel" style={{ marginBottom: 10 }} open={unknowns.length > 0 || decisions.length > 0}>
        <summary className="rowbtn" style={{ padding: 0 }}><span className="acc">▸</span> Unknowns and owned decisions ({unknowns.length + decisions.length})</summary>
        {unknowns.map((item) => (
          <p key={String(item.id)} className="tiny" style={{ margin: '6px 0 0' }}>
            <strong>{String(item.id)}</strong> — {String(item.question)} · impact: {String(item.impact)} · owner: {String(item.owner)}
          </p>
        ))}
        {decisions.map((item) => (
          <p key={String(item.id)} className="tiny" style={{ margin: '6px 0 0' }}>
            <strong>{String(item.id)}</strong> — {String(item.question)} · <em>{String(item.status)}</em>
            {item.decision ? `: ${String(item.decision)}` : ''} · owner: {String(item.owner)}
          </p>
        ))}
        {!unknowns.length && !decisions.length && <p className="tiny dim">No open unknowns or owned decisions.</p>}
      </details>

      {contradictions.length > 0 && (
        <div className="panel" style={{ marginBottom: 10, borderColor: 'var(--fail-line)' }}>
          <p className="label" style={{ marginTop: 0, color: 'var(--fail)' }}>Contradictions</p>
          {contradictions.map((item) => (
            <p key={String(item.id)} className="tiny" style={{ margin: '4px 0' }}>
              <strong>{String(item.id)}</strong> — {String(item.statement)} · owner: {String(item.owner)}
            </p>
          ))}
        </div>
      )}

      <details className="panel" style={{ marginBottom: 10 }}>
        <summary className="rowbtn" style={{ padding: 0 }}><span className="acc">▸</span> Changes since previous revision</summary>
        {!previous && <p className="tiny dim">This is the first revision.</p>}
        {previous && diff.length === 0 && <p className="tiny dim">No textual differences from r{previous.revision}.</p>}
        {diff.length > 0 && (
          <pre className="term" aria-label={`Diff from revision ${previous?.revision}`} style={{ maxHeight: 260 }}>
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
          <label className="label" htmlFor="packet-json">Packet JSON (schema v1)</label>
          <textarea
            id="packet-json"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            style={{ width: '100%', minHeight: 240, fontFamily: 'monospace', fontSize: 12 }}
          />
          <div className="field" style={{ marginTop: 8 }}>
            <label htmlFor="packet-change-request">Reason for the revision</label>
            <input id="packet-change-request" value={changeRequest} onChange={(event) => setChangeRequest(event.target.value)} placeholder="e.g. clarify refund timing" />
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button className="btn btn-go" type="button" disabled={save.isPending} onClick={() => { setActionError(null); save.mutate(); }}>
              Save revision
            </button>
            <button className="btn" type="button" onClick={() => setEditing(false)}>Cancel</button>
          </div>
          <p className="tiny dim" style={{ margin: '6px 0 0' }}>
            Saving creates a new immutable revision. Any approval prepared for the previous revision becomes stale and cannot be inherited.
          </p>
        </div>
      )}

      <div className="panel">
        <p className="label" style={{ marginTop: 0 }}>Provenance and history</p>
        <p className="tiny dim" style={{ margin: 0 }}>
          Created by {selected.created_by} at {selected.created_at}
          {selected.source_artifact_id ? ` · artifact ${selected.source_artifact_id}` : ''}
          {selected.parent_revision_id ? ` · parent ${selected.parent_revision_id}` : ''}
          {selected.change_request ? ` · change request: ${selected.change_request}` : ''}
        </p>
        {(bundle?.reviews ?? []).map((review) => (
          <p key={review.id} className="tiny" style={{ margin: '5px 0 0' }}>
            <strong>{review.outcome}</strong> by {review.actor_name} at {review.created_at}
            {review.reason ? ` — ${review.reason}` : ''} · r{review.revision} · <span className="mono">{review.content_hash.slice(0, 12)}…</span>
          </p>
        ))}
        {!(bundle?.reviews ?? []).length && <p className="tiny dim" style={{ margin: '5px 0 0' }}>No gate reviews recorded.</p>}
      </div>
      <p className="tiny dim" style={{ margin: '6px 0 0' }}>
        This portable packet export is separate from the stage conversation transcript export.
      </p>
    </section>
  );
}
