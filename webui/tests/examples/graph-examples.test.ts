import { describe, expect, it } from 'vitest';
import { buildGraph } from '../../src/views/Workflows';
import agentLoopJson from '../../../workflows/agent-loop/workflow.smg.json';
import clarifyJson from '../../../workflows/clarify-produce-review/workflow.smg.json';
import researchJson from '../../../workflows/research-synthesize-verify/workflow.smg.json';

/**
 * Optional shipped-example graph contracts. Run explicitly:
 *   cd webui && npm run test:examples
 * Not part of the default asset-free unit suite. The historical default graph
 * example is intentionally absent here; its replacement/migration belongs to
 * the Batch 4 example-workflow migration, not to this isolation baseline.
 */
type Smg = {
  name: string;
  entry: string;
  exit: string;
  flows: Record<string, { type: string; stages?: Record<string, unknown> }>;
  'flow-order': string[];
};

function workflow(path: string, value: unknown) {
  const graph = value as Smg;
  return {
    id: `wf_${path}`, name: graph.name, smg_path: path, version: 1, revision: 1,
    description: '', entry: graph.entry, exit: graph.exit,
    flow_count: graph['flow-order'].length,
    gate_count: graph['flow-order'].filter((f: string) => graph.flows[f].type === 'gate').length,
    stage_count: (Object.values(graph.flows) as { stages?: Record<string, unknown> }[]).flatMap((f) => Object.keys(f.stages || {})).length,
    created_at: '2026-10-22T00:00:00Z', lanes: [], content: graph as never,
  };
}

describe('buildGraph against shipped generic example templates', () => {
  it('shows the agent loop correction route and final result terminal', () => {
    const { nodes, edges } = buildGraph(workflow('workflows/agent-loop/workflow.smg.json', agentLoopJson) as never);
    const edge = (id: string) => edges.find((e) => e.id === id);
    expect(edge('e:f:agent-loop::s:agent-check:revise')?.target).toBe('f:agent-loop::s:agent-act');
    expect(edge('e:f:agent-loop::s:agent-check:revise')?.data?.back).toBe(true);
    expect(edge('e:f:agent-loop::s:agent-result:success')?.target).toBe('__t:_exit');
    expect(nodes.filter((n) => n.data.unreach && !n.id.startsWith('__t'))).toEqual([]);
  });

  it('shows clarify -> produce -> human gate with revision and approval routes', () => {
    const { nodes, edges } = buildGraph(workflow('workflows/clarify-produce-review/workflow.smg.json', clarifyJson) as never);
    const edge = (id: string) => edges.find((e) => e.id === id);
    expect(edge('e:f:clarify-produce-review::s:clarify:success')?.target).toBe('f:clarify-produce-review::s:produce');
    expect(edge('e:f:clarify-produce-review::s:produce:produced')?.target).toBe('f:review-gate');
    expect(nodes.some((n) => n.id === 'f:review-gate')).toBe(true);
    expect(edges.some((e) => e.id === 'e:gf:review-gate:rejected')).toBe(true);
  });

  it('shows research evidence back-edges and the verified exit', () => {
    const { nodes, edges } = buildGraph(workflow('workflows/research-synthesize-verify/workflow.smg.json', researchJson) as never);
    const edge = (id: string) => edges.find((e) => e.id === id);
    expect(edge('e:f:research-synthesize-verify::s:verify:revise')?.target).toBe('f:research-synthesize-verify::s:synthesize');
    expect(edge('e:f:research-synthesize-verify::s:verify:missing-evidence')?.target).toBe('f:research-synthesize-verify::s:research');
    expect(edge('e:f:research-synthesize-verify::s:verify:verified')?.target).toBe('__t:_exit');
    expect(nodes.filter((n) => n.data.unreach && !n.id.startsWith('__t'))).toEqual([]);
  });
});
