# TurboJET — SMG + ADC Architecture

Date: 2026-05-12
Zettelkasten: `20260512T212800270163000`, `20260512T220138129367000`, `20260512T221416564712000`

## Summary

Major architectural shift from hardcoded stages to external JSON workflow definitions using a State Machine Graph (SMG) + Agent Definition Config (ADC) pattern. Filesystem-first artifact passing, output/return-value separation, and per-stage/per-flow safety limits.

## Architecture Overview

### SMG (State Machine Graph)
- Defines flows, stages, routing paths, gates, limits
- Stages grouped into flows (discovery, planning, engineering, delivery)
- Each flow has entry/exit stages
- Routes define transitions based on stage results (cyclic allowed)
- Reserved routes: `_next-flow`, `_escalate`
- `gate: true` marks human-approval stages
- Agent references are explicit file paths

### ADC (Agent Definition Config)
- Per-agent definition file
- Input manifest (pre-stage fail guard) + output manifest (post-stage fail guard)
- Declares prompt path, tools, timeout, status schema
- Artifact manifests are validation gates, not wiring

### Engine
- State machine runner that reads SMG + ADC
- Components: SMGLoader, ADCLoader, StateTracker, ArtifactValidator, PromptBuilder, Dispatcher, Router
- Pre-checks input artifacts before starting stage
- Post-checks output artifacts after stage completes
- Enforces limits (iteration, steps, depth)

### Limits
- `maxIterations` per-stage — prevents infinite retry on one stage
- `maxSteps` per-flow — prevents infinite ping-pong between stages
- `maxDepth` global — prevents runaway agent recursion (future)
- Injected into every agent prompt at dispatch time

### Prompt Convention
- System prompt is a template with placeholders
- Engine injects: stageId, inputDir, outputDir, manifest, limits
- Agents read artifacts from disk, write to output directory
- Working agents (E8) write directly to project source
- Status schema controls the return signal to runner

### Artifact Flow
- Nested: `.turbojet/artifacts/{runId}/{stageId}/`
- Agents write freely (any format)
- Two output types: stage artifacts (plans, analysis) + project artifacts (code)
- Engine resolves paths from SMG routing at dispatch time
- No duplicate definitions — SMG defines flow, ADC defines expectations

### Directory Structure
```
.turbojet/
  workflows/
    default.smg.json
  agents/
    discovery/  intake.json, evaluate.json
    planning/   analyze.json, architect.json, design.json, align.json, plan.json, validate.json
    engineering/ ...
    delivery/   ...
  prompts/
    discovery/  intake.md, evaluate.md
    planning/   analyze.md, architect.md, ...
  schemas/
    analysis-result.json, evaluation-result.json, ...
```
