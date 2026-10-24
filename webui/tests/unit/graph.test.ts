import { describe, expect, it } from 'vitest';
import { buildGraph } from '../../src/views/Workflows';
import syntheticJson from '../fixtures/synthetic.smg.json';

type SyntheticGraph = {
  name: string;
  entry: string;
  exit: string;
  flows: Record<string, { type: string; stages?: Record<string, { routes?: Record<string, unknown> }> }>;
  'flow-order': string[];
};

const smg = syntheticJson as unknown as SyntheticGraph;
const wf = {
  id: 'wf_synthetic', name: smg.name, smg_path: 'webui/tests/fixtures/synthetic.smg.json',
  version: 1, revision: 1, description: '', entry: smg.entry, exit: smg.exit,
  flow_count: smg['flow-order'].length,
  gate_count: smg['flow-order'].filter((f) => smg.flows[f].type === 'gate').length,
  stage_count: (Object.values(smg.flows) as { stages?: Record<string, unknown> }[])
    .flatMap((f) => Object.keys(f.stages || {})).length,
  created_at: '2026-10-22T00:00:00Z', lanes: [], content: smg as never,
};

describe('buildGraph against an asset-free synthetic workflow', () => {
  const { nodes, edges } = buildGraph(wf as never);
  const edge = (id: string) => edges.find((e) => e.id === id);

  it('draws the implicit approved edge out of empty-stage gates', () => {
    expect(edge('e:gate:gate-a:approved')?.target).toBe('f:flow-b::s:finish');
  });

  it('connects flow exits across gates via _next-flow', () => {
    expect(edge('e:f:flow-a::s:start:success')?.target).toBe('f:flow-a::s:check');
    expect(edge('e:f:flow-a::s:check:success')?.target).toBe('f:flow-a::s:finish');
    expect(edge('e:f:flow-a::s:finish:success')?.target).toBe('f:gate-a');
    expect(edge('e:gf:gate-a:approved')?.target).toBe('f:flow-b::s:finish');
    expect(edge('e:f:flow-b::s:finish:success')?.target).toBe('__t:_exit');
    expect(nodes.some((node) => node.id.startsWith('f:removed'))).toBe(false);
  });

  it('marks every synthetic stage reachable', () => {
    const unreach = (label: string) => nodes.find((n) => n.data.label === label)?.data.unreach;
    expect(unreach('start')).toBeFalsy();
    expect(unreach('check')).toBeFalsy();
    expect(unreach('finish')).toBeFalsy();
    expect(nodes.filter((n) => n.data.unreach && !n.id.startsWith('__t'))).toEqual([]);
  });

  it('styles forward edges solid and feedback loops dashed/amber', () => {
    const back = (id: string) => edges.find((e) => e.id === id)?.data?.back;
    expect(back('e:f:flow-a::s:start:success')).toBe(false);
    expect(back('e:gate:gate-a:approved')).toBe(false);
    expect(back('e:f:flow-a::s:check:retry')).toBe(true);
    const retry = edges.find((e) => e.id === 'e:f:flow-a::s:check:retry');
    expect((retry?.style as Record<string, unknown>).strokeDasharray).toBeTruthy();
  });

  it('weights forward success heavier than retry/error and feedback edges', () => {
    const width = (id: string) => (edges.find((e) => e.id === id)?.style as Record<string, unknown>).strokeWidth as number;
    const hasArrow = (id: string) => Boolean(edges.find((e) => e.id === id)?.markerEnd);
    const success = width('e:f:flow-a::s:start:success');
    const retry = width('e:f:flow-a::s:check:retry');
    const error = width('e:f:flow-a::s:check:cannot-complete');
    expect(success).toBeGreaterThan(error);
    expect(success).toBeGreaterThan(retry);
    expect(retry).toBeLessThan(error);
    expect(hasArrow('e:f:flow-a::s:start:success')).toBe(true);
    expect(hasArrow('e:f:flow-a::s:check:retry')).toBe(true);
  });

  it('gives each incident edge its own connection port', () => {
    const intoCheck = edges.filter((e) => e.target === 'f:flow-a::s:check').map((e) => e.targetHandle);
    const outOfCheck = edges.filter((e) => e.source === 'f:flow-a::s:check').map((e) => e.sourceHandle);
    expect(new Set(intoCheck).size).toBe(intoCheck.length);
    expect(new Set(outOfCheck).size).toBe(outOfCheck.length);
    expect(outOfCheck.length).toBeGreaterThan(1);
  });

  it('keeps every node connected except deliberately dead stages', () => {
    const hasIn = new Set(edges.map((e) => e.target));
    const entryIds = new Set(['f:flow-a::s:start']);
    const orphans = nodes.filter((n) => !n.data.unreach && !n.id.startsWith('__t') && !hasIn.has(n.id) && !entryIds.has(n.id));
    expect(orphans.map((n) => n.data.label)).toEqual([]);
  });
});
