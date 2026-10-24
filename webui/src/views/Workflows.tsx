import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ReactFlow, Background, Controls, Handle, MarkerType, Position, addEdge, useNodesState, useEdgesState, type Edge, type Node, type NodeProps } from '@xyflow/react';
import dagre from '@dagrejs/dagre';
import '@xyflow/react/dist/style.css';
import { api } from '../api';
import type { WorkflowVersion } from '../types';
import type { Nav } from '../App';

// ---- Graph construction ----

const NODE_W = 148;
const NODE_H = 40;
const GATE_W = 128;
const TERM_W = 84;

// Distinct hues per flow, reused cyclically; gates/terminals get fixed colors.
const FLOW_HUES = [215, 155, 25, 285, 190, 340, 60, 120];

interface GNode { id: string; label: string; sub?: string; w: number; hue: number | null; gate: boolean; term: boolean; entry: boolean }

function flowEntryNodeId(content: WorkflowVersion['content'], flow: string): string | null {
  const def = content.flows?.[flow];
  if (!def) return null;
  if (!def.stages || Object.keys(def.stages).length === 0) return `f:${flow}`; // gate flow = single node
  return `f:${flow}::s:${def.entry || Object.keys(def.stages)[0]}`;
}

export function buildGraph(wf: WorkflowVersion): { nodes: Node[]; edges: Edge[] } {
  const content = wf.content;
  const order = content['flow-order'] || Object.keys(content.flows || {});
  const flows = content.flows || {};
  const nodes: GNode[] = [];
  const edges: Edge[] = [];
  const hueOf = new Map<string, number>();
  order.forEach((f, i) => hueOf.set(f, FLOW_HUES[i % FLOW_HUES.length]));

  const pushStage = (flow: string, stage: string, entry: boolean) => {
    const id = `f:${flow}::s:${stage}`;
    if (!nodes.some((n) => n.id === id)) {
      nodes.push({ id, label: stage, sub: flow, w: NODE_W, hue: hueOf.get(flow) ?? null, gate: false, term: false, entry });
    }
    return id;
  };
  const pushFlowNode = (flow: string) => {
    const id = `f:${flow}`;
    if (!nodes.some((n) => n.id === id)) {
      nodes.push({ id, label: flow, sub: 'gate', w: GATE_W, hue: null, gate: true, term: false, entry: order[0] === flow });
    }
    return id;
  };
  const pushTerminal = (name: string) => {
    const id = `__t:${name}`;
    if (!nodes.some((n) => n.id === id)) {
      nodes.push({ id, label: name.replace(/^_/, ''), w: TERM_W, hue: null, gate: false, term: true, entry: false });
    }
    return id;
  };
  const nodeForFlow = (flow: string): string | null => {
    const def = flows[flow];
    if (!def) return null;
    return def.stages && Object.keys(def.stages).length > 0 ? flowEntryNodeId(content, flow) : pushFlowNode(flow);
  };
  const resolve = (fromFlow: string, target: unknown): string | null => {
    if (typeof target === 'string') {
      if (target === '_next-flow') {
        const idx = order.indexOf(fromFlow);
        const next = idx >= 0 ? order[idx + 1] : undefined;
        if (!next) return pushTerminal(wf.exit || '_exit');
        return nodeForFlow(next) ?? pushTerminal(`_to:${next}`);
      }
      if (target.startsWith('_')) return pushTerminal(target);
      const def = flows[fromFlow];
      if (def?.stages && target in def.stages) return pushStage(fromFlow, target, false);
      if (flows[target]) return nodeForFlow(target); // direct flow jump
      return pushTerminal(target);
    }
    const t = target as { flow?: string; stage?: string };
    if (t?.flow && t?.stage) {
      const def = flows[t.flow];
      if (def?.stages && Object.keys(def.stages).length > 0) return pushStage(t.flow, t.stage, false);
      return nodeForFlow(t.flow) ?? pushTerminal(t.flow);
    }
    return null;
  };

  // Ensure every stage of every flow exists as a node.
  for (const flow of order) {
    const def = flows[flow];
    if (!def) continue;
    if (!def.stages || Object.keys(def.stages).length === 0) { pushFlowNode(flow); continue; }
    const entryStage = def.entry || Object.keys(def.stages)[0];
    for (const stage of Object.keys(def.stages)) pushStage(flow, stage, stage === entryStage);
  }

  // Edges from stage routes and flow-level routes.
  for (const flow of order) {
    const def = flows[flow];
    if (!def) continue;
    const hasStages = def.stages && Object.keys(def.stages).length > 0;
    if (hasStages) {
      for (const [stage, sdef] of Object.entries(def.stages!)) {
        for (const [outcome, target] of Object.entries(sdef.routes || {})) {
          const to = resolve(flow, target);
          if (to) edges.push({ id: `e:f:${flow}::s:${stage}:${outcome}`, source: `f:${flow}::s:${stage}`, target: to, label: outcome });
        }
      }
      // Flow-level routes hang off the flow's exit stage.
      const exitStage = def.exit || Object.keys(def.stages!).slice(-1)[0];
      for (const [outcome, target] of Object.entries(def.routes || {})) {
        const to = resolve(flow, target);
        if (to) edges.push({ id: `e:fx:${flow}:${outcome}`, source: `f:${flow}::s:${exitStage}`, target: to, label: outcome });
      }
    } else {
      for (const [outcome, target] of Object.entries(def.routes || {})) {
        const to = resolve(flow, target);
        if (to) edges.push({ id: `e:gf:${flow}:${outcome}`, source: `f:${flow}`, target: to, label: outcome });
      }
    }
  }

  // Gates with no stages are manager-level pauses: after approval the manager
  // advances to the next flow in flow-order — draw that implicit edge.
  order.forEach((flow, i) => {
    const def = flows[flow];
    if (!def || (def.stages && Object.keys(def.stages).length > 0)) return;
    const next = order[i + 1];
    const to = next ? nodeForFlow(next) : pushTerminal(wf.exit || '_exit');
    if (to) edges.push({ id: `e:gate:${flow}:approved`, source: `f:${flow}`, target: to, label: 'approved' });
  });

  // Reachability from the flow entries: stages no route leads to never execute.
  const adj = new Map<string, string[]>();
  for (const e of edges) {
    if (!adj.has(e.source)) adj.set(e.source, []);
    adj.get(e.source)!.push(e.target);
  }
  const reachable = new Set<string>();
  const stack = order.map((f) => nodeForFlow(f)).filter((x): x is string => !!x);
  while (stack.length) {
    const id = stack.pop()!;
    if (reachable.has(id)) continue;
    reachable.add(id);
    for (const t of adj.get(id) || []) stack.push(t);
  }

  // Layout, two passes: first rank uniformly to classify feedback edges, then
  // re-rank with forward edges heavily weighted so they always run left-to-right
  // and only feedback edges loop back.
  const rank = () => {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: 'LR', nodesep: 26, ranksep: 68, marginx: 16, marginy: 16 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const n of nodes) g.setNode(n.id, { width: n.w, height: NODE_H });
    edges.forEach((e) => g.setEdge(e.source, e.target));
    dagre.layout(g);
    return g;
  };
  const g1 = rank();
  const rankOf = new Map<string, number>();
  for (const n of nodes) rankOf.set(n.id, (g1.node(n.id)?.rank as number | undefined) ?? 0);
  const isBack = (e: { source: string; target: string }) =>
    e.source === e.target || (rankOf.get(e.target) ?? 0) < (rankOf.get(e.source) ?? 0);
  const g = rank(); // second pass below sets per-edge weights
  g.setDefaultEdgeLabel(() => ({}));
  edges.forEach((e) => {
    g.setEdge(e.source, e.target, { weight: isBack(e) ? 1 : 4 });
  });
  dagre.layout(g);

  const outOf = new Map<string, string[]>();
  const into = new Map<string, string[]>();
  for (const e of edges) {
    (outOf.get(e.source) ?? outOf.set(e.source, []).get(e.source))!.push(e.id);
    (into.get(e.target) ?? into.set(e.target, []).get(e.target))!.push(e.id);
  }

  const rfNodes: Node[] = nodes.map((n) => {
    const pos = g.node(n.id);
    const unreach = !n.term && !n.gate && !reachable.has(n.id);
    return {
      id: n.id,
      position: { x: (pos?.x ?? 0) - n.w / 2, y: (pos?.y ?? 0) - NODE_H / 2 },
      data: { label: n.label, src: outOf.get(n.id) || [], tgt: into.get(n.id) || [], entry: n.entry, unreach },
      type: 'stage',
      style: {
        width: n.w,
        height: NODE_H,
        borderRadius: 10,
        border: unreach
          ? '1px dashed var(--faint)'
          : `1px solid ${n.term ? 'var(--rule-strong)' : `oklch(70% 0.12 ${n.hue ?? 0})`}`,
        background: n.term ? 'var(--surface)' : n.gate ? 'var(--wait-bg)' : 'var(--surface)',
        color: n.term ? 'var(--faint)' : 'var(--ink)',
        opacity: unreach ? 0.65 : 1,
        fontSize: 12,
        padding: '0 8px',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        boxShadow: n.entry ? `0 0 0 2px oklch(70% 0.12 ${n.hue ?? 220})` : undefined,
      },
    };
  });
  const BACK_STROKE = 'oklch(68% 0.13 65)';
  const FWD_STROKE = 'oklch(45% 0.02 240)';   // happy path: heavy
  const ERR_STROKE = '#B06A28';               // retry/error forward: light, warm
  const ERROR_OUTCOMES = new Set(['failed', 'cannot-complete', 'reject', 'rejected', 'needs-clarification', 'revise', 'missing-evidence', 'missing', 'needs-input']);
  const rfEdges: Edge[] = edges.map((e) => {
    const back = isBack(e);
    const error = !back && typeof e.label === 'string' && ERROR_OUTCOMES.has(e.label);
    const kind = back ? 'back' : error ? 'error' : 'ok';
    const [stroke, width, dash] = back
      ? [BACK_STROKE, 1.2, '6 4']
      : error
        ? [ERR_STROKE, 1.4, undefined]
        : [FWD_STROKE, 2.6, undefined];
    return {
      ...e,
      sourceHandle: e.id,
      targetHandle: e.id,
      data: { back, kind },
      type: back ? 'default' : 'smoothstep',
      animated: false,
      labelStyle: { fontSize: 10, fill: back ? BACK_STROKE : error ? ERR_STROKE : 'var(--muted)', fontWeight: kind === 'ok' ? 600 : 400 },
      labelBgStyle: { fill: 'var(--surface)', fillOpacity: 0.9 },
      style: { stroke, strokeWidth: width, strokeDasharray: dash },
      markerEnd: { type: MarkerType.ArrowClosed, color: stroke, width: 14, height: 14 },
    };
  });
  return { nodes: rfNodes, edges: rfEdges };
}

interface NfData { label: string; src: string[]; tgt: string[] }

function StageNode({ data }: NodeProps) {
  const d = data as unknown as NfData;
  return (
    <>
      {d.tgt.map((id, i, a) => (
        <Handle key={id} type="target" position={Position.Left} id={id} isConnectableStart={false}
          style={{ top: `${((i + 1) / (a.length + 1)) * 100}%`, left: -4, width: 6, height: 6, borderRadius: 9999, background: 'var(--surface)', border: '1.5px solid var(--rule-strong)' }} />
      ))}
      <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: '100%' }}>{d.label}</span>
      {d.src.map((id, i, a) => (
        <Handle key={id} type="source" position={Position.Right} id={id}
          style={{ top: `${((i + 1) / (a.length + 1)) * 100}%`, right: -4, width: 6, height: 6, borderRadius: 9999, background: 'var(--surface)', border: '1.5px solid var(--rule-strong)' }} />
      ))}
    </>
  );
}
const nodeTypes = { stage: StageNode };

function GraphView({ wf }: { wf: WorkflowVersion }) {
  const initial = useMemo(() => buildGraph(wf), [wf]);
  const [nodes, setNodes, onNodesChange] = useNodesState(initial.nodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initial.edges);
  const reset = () => {
    const g = buildGraph(wf);
    setNodes(g.nodes);
    setEdges(g.edges);
  };
  if (!initial.nodes.length) return <p className="tiny dim">This workflow has no flows.</p>;
  return (
    <>
      <p className="tiny dim" style={{ margin: '4px 0 4px' }}>
        <span style={{ display: 'inline-flex', gap: 10, alignItems: 'center', flexWrap: 'wrap' }}>
          <span style={{ display: 'inline-flex', gap: 4, alignItems: 'center' }}><svg width="26" height="6" aria-hidden><defs><marker id="lg-ok" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto"><path d="M0,0L6,3L0,6Z" fill="oklch(45% 0.02 240)" /></marker></defs><line x1="0" y1="3" x2="22" y2="3" stroke="oklch(45% 0.02 240)" strokeWidth="2.6" markerEnd="url(#lg-ok)" /></svg>forward success</span>
          <span style={{ display: 'inline-flex', gap: 4, alignItems: 'center' }}><svg width="26" height="6" aria-hidden><defs><marker id="lg-err" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto"><path d="M0,0L6,3L0,6Z" fill="#B06A28" /></marker></defs><line x1="0" y1="3" x2="22" y2="3" stroke="#B06A28" strokeWidth="1.4" markerEnd="url(#lg-err)" /></svg>retry / error</span>
          <span style={{ display: 'inline-flex', gap: 4, alignItems: 'center' }}><svg width="26" height="6" aria-hidden><defs><marker id="lg-bk" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto"><path d="M0,0L6,3L0,6Z" fill="oklch(68% 0.13 65)" /></marker></defs><line x1="0" y1="3" x2="22" y2="3" stroke="oklch(68% 0.13 65)" strokeWidth="1.2" strokeDasharray="6 4" markerEnd="url(#lg-bk)" /></svg>feedback loop</span>
          <span style={{ display: 'inline-flex', gap: 4, alignItems: 'center' }}><svg width="20" height="6" aria-hidden><rect x="1" y="0.5" width="18" height="5" fill="none" stroke="var(--faint)" strokeDasharray="3 2" /></svg>never routed to</span>
          · Sandbox: drag nodes, draw routes from a node rim, select + Backspace to delete — local to this view, nothing is saved.
        </span>
        <button className="tbtn" style={{ marginLeft: 8 }} onClick={reset}>Reset layout</button>
      </p>
      <div className="graphbox">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onConnect={(c) => setEdges((eds) => addEdge({ ...c, type: 'smoothstep', labelStyle: { fontSize: 10, fill: 'var(--muted)' }, labelBgStyle: { fill: 'var(--surface)', fillOpacity: 0.9 }, style: { stroke: 'var(--rule-strong)' } }, eds))}
          fitView
          nodeTypes={nodeTypes}
          proOptions={{ hideAttribution: true }}
          minZoom={0.2}
        >
          <Background color="var(--rule)" gap={18} />
          <Controls />
        </ReactFlow>
      </div>
    </>
  );
}

// ---- Detail panel ----

function FlowAccordion({ wf }: { wf: WorkflowVersion }) {
  const order = wf.content['flow-order'] || Object.keys(wf.content.flows || {});
  const flows = wf.content.flows || {};
  const [openFlow, setOpenFlow] = useState<string | null>(order[0] ?? null);
  return (
    <div>
      {order.map((name) => {
        const def = flows[name];
        if (!def) return null;
        const gate = def.type === 'gate';
        const stages = Object.entries(def.stages || {});
        const open = openFlow === name;
        return (
          <div key={name} style={{ border: '1px solid var(--rule)', borderRadius: 'var(--r)', marginBottom: 6, overflow: 'hidden', background: open ? 'var(--sunk)' : 'transparent', transition: 'background 160ms ease' }}>
            <button
              className="rowbtn"
              style={{ padding: '7px 10px', gap: 8, borderBottom: open ? '1px solid var(--rule)' : '1px solid transparent', width: '100%' }}
              onClick={() => setOpenFlow(open ? null : name)}
              aria-expanded={open}
            >
              <span className={`acc${open ? ' acc-open' : ''}`}>▸</span>
              <span className={`chip ${gate ? 'c-wait' : 'c-idle'}`}>{gate ? 'gate' : 'flow'}</span>
              <span style={{ fontWeight: 600, fontSize: 13, flex: 1, textAlign: 'left' }}>{name}</span>
              <span className="tstamp">{stages.length} stage{stages.length === 1 ? '' : 's'}</span>
            </button>
            {open && (
              <div style={{ padding: '4px 10px 10px' }}>
                {stages.length ? stages.map(([sname, sdef]) => (
                  <div key={sname} className="evt">
                    <span className="dot d-idle" />
                    <span style={{ flex: 1, minWidth: 0 }}>
                      {sname}
                      {sdef.interactive && <span className="chip c-wait" style={{ marginLeft: 6 }}>interactive</span>}
                    </span>
                    <span className="tiny dim mono" style={{ fontSize: 10.5, maxWidth: '40%', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {(sdef.agent || '').split('/').slice(-2, -1)[0] || sdef.agent}
                    </span>
                  </div>
                )) : <p className="tiny dim" style={{ margin: '4px 0 0' }}>No stages — gate flows pause between flows.</p>}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function LanesSection({ wf }: { wf: WorkflowVersion }) {
  const qc = useQueryClient();
  const allLanes = useQuery({ queryKey: ['lanes'], queryFn: api.lanes });
  const [pick, setPick] = useState('');
  const lanes = allLanes.data?.lanes || [];
  const unpinned = lanes.filter((l) => l.workflow_id !== wf.id);
  const pin = useMutation({
    mutationFn: (laneId: string) => api.setLaneWorkflow(laneId, wf.id),
    onSuccess: () => { qc.invalidateQueries({ queryKey: ['lanes'] }); qc.invalidateQueries({ queryKey: ['workflows'] }); },
  });
  return (
    <div style={{ marginTop: 14 }}>
      <p className="label">Lanes pinned to this version</p>
      {wf.lanes.length ? wf.lanes.map((l) => (
        <div key={l.id} className="evt">
          <span className="chip c-idle" style={{ flexShrink: 0 }}>{l.name}</span>
          <span style={{ flex: 1, minWidth: 0 }} className="tiny dim">{l.project_id}</span>
          <span className="tiny dim mono">{wf.id}</span>
        </div>
      )) : <p className="tiny dim">No lanes pinned to this immutable version.</p>}
      {unpinned.length > 0 && (
        <div style={{ display: 'flex', gap: 7, marginTop: 8 }}>
          <select style={{ flex: 1 }} value={pick} onChange={(e) => setPick(e.target.value)}>
            <option value="">Pin a lane to this version…</option>
            {unpinned.map((l) => <option key={l.id} value={l.id}>{l.name} ({l.project_id.slice(0, 13)}…)</option>)}
          </select>
          <button className="btn btn-go" disabled={!pick || pin.isPending} onClick={() => { pin.mutate(pick); setPick(''); }}>Pin</button>
        </div>
      )}
      {pin.isError && <p className="tiny" style={{ color: 'var(--fail)' }}>{(pin.error as Error).message}</p>}
    </div>
  );
}

export function WorkflowDetailPage({ nav, wfId }: { nav: Nav; wfId: string }) {
  const q = useQuery({ queryKey: ['workflow', wfId], queryFn: () => api.workflow(wfId) });
  const list = useQuery({ queryKey: ['workflows'], queryFn: api.workflows });
  const qc = useQueryClient();
  const [templateMsg, setTemplateMsg] = useState<string | null>(null);
  const useTemplate = useMutation({
    mutationFn: (workflowId: string) => api.createProject(`${q.data?.name || 'Workflow'} project`, { workflow_id: workflowId }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['projects'] });
      qc.invalidateQueries({ queryKey: ['lanes'] });
      setTemplateMsg('Project created with this workflow. Start a run from the Runs view.');
    },
    onError: (error) => setTemplateMsg((error as Error).message),
  });
  const [open, setOpen] = useState(false);
  if (q.isLoading) return <div className="card pad tiny dim">Loading workflow…</div>;
  if (q.error || !q.data) return <div className="card pad tiny" style={{ color: 'var(--fail)' }}>Failed to load workflow: {(q.error as Error)?.message}</div>;
  const wf = q.data;
  const versions = (list.data?.workflows || []).filter((x) => x.name === wf.name);
  return (
    <>
      <div className="crumb">
        <button onClick={() => nav.setView('workflows')}>← Workflows</button>
        <span className="sep">/</span>
        <span className="here">{wf.name}</span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 8, alignItems: 'center' }}>
          {versions.length > 1 && (
            <select aria-label="Version" value={wf.id} onChange={(e) => nav.openWorkflow(e.target.value)}>
              {versions.map((v) => <option key={v.id} value={v.id}>v{v.version} · rev {v.revision}{v.id === wf.id ? ' (this)' : ''}</option>)}
            </select>
          )}
          <button className="tbtn" onClick={() => setOpen(true)}>Start a run…</button>
          <button className="tbtn" disabled={useTemplate.isPending}
            onClick={() => useTemplate.mutate(wf.id)}>Create project from this version</button>
        </span>
      </div>
      {templateMsg && <p className="tiny" role="status" style={{ margin: '0 0 6px', color: useTemplate.isError ? 'var(--fail)' : 'var(--done)' }}>{templateMsg}</p>}
      <div className="card pad">
        <WorkflowDetail wf={wf} />
      </div>
      {open && <StartRunModal nav={nav} onClose={() => setOpen(false)} />}
    </>
  );
}

function WorkflowDetail({ wf }: { wf: WorkflowVersion }) {
  return (
    <>
      <div className="stepmeta" style={{ flexWrap: 'wrap' }}>
        <span className="chip c-done">v{wf.version}</span>
        <span className={`chip ${wf.maturity === 'supported' ? 'c-done' : 'c-wait'}`}>
          {wf.maturity === 'supported' ? 'supported' : 'experimental'}
        </span>
        <span className="chip c-idle">rev {wf.revision}</span>
        <span className="tiny dim mono">{wf.catalog_id || wf.source_rel_path || wf.smg_path.split('/').slice(-1)[0]}</span>
        <span className="tiny dim">{relDate(wf.created_at)}</span>
      </div>
      <h2 style={{ marginBottom: 4 }}>{wf.name}</h2>
      {wf.description && <p className="tiny dim" style={{ margin: '0 0 10px' }}>{wf.description}</p>}
      <div style={{ display: 'flex', gap: 22, flexWrap: 'wrap', margin: '8px 0 12px' }}>
        <span><strong style={{ fontSize: 19, display: 'block' }}>{wf.flow_count}</strong><span className="tiny dim">flows</span></span>
        <span><strong style={{ fontSize: 19, display: 'block' }}>{wf.gate_count}</strong><span className="tiny dim">gates</span></span>
        <span><strong style={{ fontSize: 19, display: 'block' }}>{wf.stage_count}</strong><span className="tiny dim">stages</span></span>
        <span><strong style={{ fontSize: 13, display: 'block', marginTop: 4 }}>{wf.entry} → {wf.exit}</strong><span className="tiny dim">entry → exit</span></span>
      </div>

      <p className="label">Execution graph — stages, routes and loops</p>
      <GraphView key={`graph:${wf.id}`} wf={wf} />

      <p className="label" style={{ marginTop: 14 }}>Flows in execution order</p>
      <FlowAccordion key={`flows:${wf.id}`} wf={wf} />

      <LanesSection wf={wf} />

      <p className="tiny dim" style={{ marginTop: 14 }}>Versions are immutable — new content is registered as a new revision. Runs record the pinned version of their lane.</p>
    </>
  );
}

function relDate(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

// ---- View ----

export function Workflows({ nav }: { nav: Nav }) {
  const qc = useQueryClient();
  const wfs = useQuery({ queryKey: ['workflows'], queryFn: api.workflows });
  const [open, setOpen] = useState(false);
  const rescan = useMutation({
    mutationFn: api.importWorkflows,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['workflows'] }),
  });

  if (wfs.isLoading) return <div className="card pad tiny dim">Loading workflow registry…</div>;
  if (wfs.error) return <div className="card pad tiny dim" style={{ color: 'var(--fail)' }}>Failed to load: {(wfs.error as Error).message}</div>;
  const versions = wfs.data!.workflows;

  return (
    <>
      <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'center', marginBottom: 8 }}>
        <h2 style={{ margin: 0 }}>Workflows</h2>
        <span className="tiny dim">{versions.length} registered version{versions.length === 1 ? '' : 's'} — immutable, content-addressed</span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 8 }}>
          <button className={`tbtn ${rescan.isPending ? 'on' : ''}`} onClick={() => rescan.mutate()}>Rescan config</button>
          <button className="tbtn" onClick={() => setOpen(true)}>Start a run…</button>
        </span>
      </div>
      {rescan.error && <p className="tiny" style={{ color: 'var(--fail)' }}>Rescan failed: {(rescan.error as Error).message}</p>}
      <div className="card pad" style={{ padding: '4px 0' }}>
        {!versions.length && <div className="tiny dim" style={{ padding: '0 14px' }}>
          <p style={{ margin: '0 0 4px' }}>No workflows registered yet. Add a <span className="mono">.smg.json</span> graph with a catalog block under a configured workflow source, then rescan or restart.</p>
          <p style={{ margin: 0 }}>The service still serves HTTP and Chief; runs and project creation stay disabled until a workflow is installed.</p>
        </div>}
        {versions.map((w, wi) => {
          const maxRev = Math.max(...versions.filter((x) => x.name === w.name).map((x) => x.revision));
          return (
            <div key={w.id} className="runrow" style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 14px', borderBottom: wi < versions.length - 1 ? '1px solid var(--rule)' : undefined }}>
              <button
                className="rowbtn"
                style={{ flex: 1, minWidth: 0, borderBottom: 0, padding: 0, gap: 8 }}
                onClick={() => nav.openWorkflow(w.id)}
              >
                <span style={{ flex: 1, minWidth: 0, textAlign: 'left' }}>
                  <span style={{ display: 'block', fontWeight: 600, fontSize: 13, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{w.name}</span>
                  <span className="tstamp">v{w.version} · rev {w.revision} · {w.flow_count} flows · {w.gate_count} gates · {w.stage_count} stages{w.lanes.length ? ` · ${w.lanes.length} lane${w.lanes.length === 1 ? '' : 's'} pinned` : ''}</span>
                </span>
                {w.catalog_id && <span className="chip c-idle">{w.catalog_label || w.catalog_id}</span>}
                <span className={`chip ${w.maturity === 'supported' ? 'c-done' : 'c-wait'}`}>
                  {w.maturity === 'supported' ? 'supported' : 'experimental'}
                </span>
                {maxRev === w.revision && <span className="chip c-done">latest</span>}
              </button>
              <span className="tstamp" style={{ width: 64, textAlign: 'right' }}>{relDate(w.created_at)}</span>
            </div>
          );
        })}
      </div>

      {open && <StartRunModal nav={nav} onClose={() => setOpen(false)} />}
    </>
  );
}

function StartRunModal({ nav, onClose }: { nav: Nav; onClose: () => void }) {
  const qc = useQueryClient();
  const [desc, setDesc] = useState('');
  const [projectId, setProjectId] = useState('');
  const [laneId, setLaneId] = useState('');
  const [msg, setMsg] = useState<string | null>(null);

  const projects = useQuery({ queryKey: ['projects'], queryFn: api.projects });
  const plist = projects.data || [];
  const laneList = useQuery({ queryKey: ['lanes'], queryFn: api.lanes });
  const lanes = (laneList.data?.lanes || []).filter((l) => !projectId || l.project_id === projectId);
  const effectiveLane = laneId && lanes.some((l) => l.id === laneId) ? laneId : lanes[0]?.id || '';
  const start = useMutation({
    mutationFn: () => api.createRun(desc.trim(), projectId || plist[0]?.id || '', effectiveLane),
    onSuccess: (r) => { onClose(); qc.invalidateQueries({ queryKey: ['runs'] }); nav.openRun(r.id); },
    onError: (e) => setMsg((e as Error).message),
  });

  return (
    <div className="tw" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="tw-box" role="dialog" aria-modal="true">
        <h2>Start a run</h2>
        <p className="tiny dim" style={{ margin: '0 0 12px' }}>The run executes on the lane you pick — including its pinned workflow version.</p>
            <div className="field">
              <label htmlFor="wp">Project</label>
              <select id="wp" value={projectId || plist[0]?.id || ''} onChange={(e) => setProjectId(e.target.value)}>
                {!plist.length && <option value="">No projects yet — create one on the dashboard</option>}
                {plist.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
              </select>
            </div>
            <div className="field">
              <label htmlFor="wl">Lane</label>
              <select id="wl" value={effectiveLane} onChange={(e) => setLaneId(e.target.value)}>
                {!lanes.length && <option value="">No lanes for this project</option>}
                {lanes.map((l) => <option key={l.id} value={l.id}>{l.name}{l.workflow_id ? ' · pinned' : ''}</option>)}
              </select>
            </div>
            <div className="field">
              <label htmlFor="wd">Description</label>
              <input id="wd" value={desc} onChange={(e) => setDesc(e.target.value)} placeholder="What should this run do?" />
            </div>
            {msg && <p className="tiny" style={{ color: 'var(--fail)' }}>{msg}</p>}
            <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
              <button className="btn btn-go" disabled={!desc.trim() || !plist.length || !lanes.length || start.isPending} onClick={() => start.mutate()}>Start run</button>
              <button className="btn" onClick={onClose}>Cancel</button>
            </div>
          </div>
        </div>
  );
}
