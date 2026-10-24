import fs from 'node:fs';
import path from 'node:path';

/**
 * Write a disposable neutral planning-capability package. It is derived at
 * test runtime, never read from a tracked workflow fixture, and uses synthetic
 * names. The production loader still validates the real capability contract.
 */
export function writeNeutralPlanningPackage(root: string): void {
  const workflow = {
    version: 1,
    name: 'Neutral planning capability package',
    description: 'Runtime-synthesized neutral package for the planning journey.',
    maturity: 'experimental',
    entry: 'collect',
    exit: 'shape-gate',
    limits: { maxIterations: 3, maxSteps: 20, maxDepth: 5 },
    'flow-order': ['collect', 'collect-gate', 'shape', 'shape-gate'],
    flows: {
      collect: {
        type: 'flow', entry: 'writer', exit: 'writer', merge_strategy: 'none',
        stages: {
          writer: { agent: 'writer', interactive: false, routes: { generated: '_next-flow' } },
        },
      },
      'collect-gate': {
        type: 'gate', routes: { approved: '_next-flow', rejected: '_rework' }, stages: {},
      },
      shape: {
        type: 'flow', entry: 'survey', exit: 'check', merge_strategy: 'none',
        stages: {
          survey: { agent: 'surveyor', routes: { success: 'outline', 'cannot-complete': '_escalate' } },
          outline: { agent: 'outliner', routes: { success: 'sketch', 'cannot-complete': '_escalate' } },
          sketch: { agent: 'sketcher', routes: { success: 'align', 'cannot-complete': '_escalate' } },
          align: { agent: 'aligner', routes: { success: 'plan', 'cannot-complete': '_escalate' } },
          plan: { agent: 'planner', routes: { success: 'check', 'cannot-complete': '_escalate' } },
          check: { agent: 'checker', routes: { approved: '_next-flow', rejected: 'plan' } },
        },
      },
      'shape-gate': {
        type: 'gate', routes: { approved: '_next-flow', rejected: '_rework' }, stages: {},
      },
    },
    catalog: { id: 'neutral-planning', label: 'Neutral planning capability' },
    capabilities: {
      schema_version: 1,
      workspace: { kind: 'none' },
      gates: { 'collect-gate': 'packet', 'shape-gate': 'plan' },
      requirements_packet: {
        produce_flow: 'collect',
        producer_stage: 'writer',
        binding_gate: 'collect-gate',
      },
      task_plan: {
        produce_flow: 'shape',
        validate_stage: 'check',
        binding_gate: 'shape-gate',
        source_packet: true,
        task_selection: { after_gate: 'shape-gate', required: true },
      },
    },
  };

  const agents: Array<{ id: string; verdicts: string[]; write: boolean }> = [
    { id: 'writer', verdicts: ['generated'], write: true },
    { id: 'surveyor', verdicts: ['success', 'cannot-complete'], write: false },
    { id: 'outliner', verdicts: ['success', 'cannot-complete'], write: false },
    { id: 'sketcher', verdicts: ['success', 'cannot-complete'], write: false },
    { id: 'aligner', verdicts: ['success', 'cannot-complete'], write: false },
    { id: 'planner', verdicts: ['success', 'cannot-complete'], write: false },
    { id: 'checker', verdicts: ['approved', 'rejected'], write: false },
  ];

  fs.mkdirSync(root, { recursive: true });
  fs.writeFileSync(path.join(root, 'workflow.smg.json'), JSON.stringify(workflow, null, 2));
  for (const agent of agents) {
    const directory = path.join(root, 'agents', agent.id);
    fs.mkdirSync(directory, { recursive: true });
    fs.writeFileSync(path.join(directory, 'agent.json'), JSON.stringify({
      id: agent.id,
      name: `Neutral ${agent.id}`,
      prompt: 'prompt.md',
      schema: `schemas/${agent.id}.json`,
      max_output_retries: 3,
      output: { artifacts: [{ name: 'output.md', required: true, verify: 'non-empty' }] },
      tools: { read: true, write: agent.write, bash: false, edit: false },
      timeout: 300,
    }, null, 2));
    fs.writeFileSync(path.join(directory, 'prompt.md'), `Neutral ${agent.id} stage prompt.\n`);
    fs.mkdirSync(path.join(root, 'schemas'), { recursive: true });
    fs.writeFileSync(path.join(root, 'schemas', `${agent.id}.json`), JSON.stringify({
      type: 'object',
      required: ['verdict', 'summary'],
      properties: {
        verdict: { type: 'string', enum: agent.verdicts },
        summary: { type: 'string' },
        content: { type: 'string' },
      },
    }, null, 2));
  }
}
