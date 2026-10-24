# TurboJET — Autonomous SDLC Orchestrator

> Zettelkasten Hub: `20260503T194400802612000`

## Problem
Software projects fail by solving the wrong problem. Every AI dev tool assumes crystal-clear requirements and only addresses coding. Nobody handles the messy upstream work where projects actually succeed or fail.

## Solution
A standalone SDLC orchestration system covering the full lifecycle — from initial customer contact through production monitoring — with AI-assisted Discovery, Planning, Engineering, and Delivery.

## Canonical Pipeline v4 (4 Phases, 17 Stages, 3 Parallel Streams)

```
PHASE 1: DISCOVERY — Initial
  D1. INTAKE          Gather customer needs, artifacts (multi-source)
  D2. EVALUATE        Feasibility, alignment, general plan ← GATE: Go/No-Go

PHASE 2: PLANNING — Detailed (3 parallel streams + alignment)
  ┌──────────────────┬──────────────────┬──────────────────┐
  │  P1. ANALYZE     │  P2. ARCHITECT   │  P2b. DESIGN     │
  │  BA: Requirements│  Architect: Sys  │  UX/UI: Visual   │
  │  Epics, Stories  │  Tech, API, Data │  Mockups, System │
  └────────┬─────────┴────────┬─────────┴────────┬─────────┘
           └──────────────────┼───────────────────┘
                              ▼
                      P3. ALIGN (3-way sync)
                              ▼
                      P4. PLAN (sprint plan)
                              ▼
                      P5. VALIDATE ← GATE: Backlog + Blueprint + Designs

PHASE 3: ENGINEERING (Sprint Execution)
  6. ANALYSIS         Codebase impact per task
  7. DESIGN           Task-level implementation plan
  8. IMPLEMENT        Write code (OpenCode / CodingTool)
  9. TEST             Generate + run tests, failure → IMPLEMENT
 10. REVIEW           Quality review (existing sdlc-flows PR review)
 11. ACCEPTANCE       Verify acceptance criteria ← GATE: Story accepted
  Sprint loop: 6→7→8→9→10→11 per task, multiple tasks in parallel

PHASE 4: DELIVERY
 12. INTEGRATE        Merge, conflict resolution, CI on combined state
 13. RELEASE          Build, deploy (staging=auto, prod=human-confirmed)
 14. VERIFY           Post-deploy validation, rollback if needed
 15. OBSERVE          Continuous monitoring → feedback to D1/P1
```

### Phase Gate Artifacts
| Gate | Required |
|---|---|
| Go/No-Go (D2→P1) | Business case, feasibility, general plan |
| Plan Approval (P5→6) | Backlog + Blueprint + Designs + Sprint plan |
| Story Acceptance (11→12) | Verified impl meeting criteria AND design fidelity |
| Release (15→done) | Deployed, verified, monitoring active |

### Design Stream — Dual Mode
- **Ingress**: External designs provided (customer, in-house designers)
- **Generated**: TurboJET produces designs from requirements
- **Hybrid**: Partial ingress, AI fills gaps
- Same P5 gate regardless of design source

### Engineering Sprint Lifecycle
```
TODO → IN_ANALYSIS → IN_DESIGN → IN_PROGRESS → IN_TEST → IN_REVIEW → ACCEPTANCE → DONE
                                                                ↑                     │
                                                                └── BLOCKED ←─────────┘
```
- Multiple stories can be in different stages simultaneously
- Dependencies enforced by sprint plan (P4)
- Feedback loops: TEST→IMPLEMENT, REVIEW→IMPLEMENT, ACCEPTANCE→DESIGN
- Sprint retrospective signals auto-detected (frequent failures at specific stages = systemic issues)

### Delivery Orchestration
- Per-story, per-sprint, per-release, or hotfix cadence
- Staging autonomous → Production human-confirmed
- Rollback triggered by: error rate, health checks, performance thresholds
- OBSERVE feeds back to INTAKE: bugs, feedback, metrics, anomalies → new work items

### Adaptive Paths
| Item Type | Path |
|---|---|
| New product (full) | D1→D2→P1+P2+P2b→P3→P4→P5→sprint loop→12→13→14→15 |
| New feature (UI) | D1→D2→P1+P2b→P3→P5→sprint loop→12→13→14→15 |
| New feature (API) | D1→D2→P1+P2→P3→P5→sprint loop→12→13→14→15 |
| Bug from prod | D1→D2→6→8→9→10→11→12→13→14→15 |
| Typo fix | D1→6→8→9→10→12→13→14→15 |
| Hotfix | 6→8→9→10→12→13→14 |
| Refactor | D1→D2→P2→P5→6→8→9→10→12→13→14→15 |

## Existing Implementation
- **sdlc-flows** (`~/Dev/sdlc-flows`) — Bash framework, production PR review (Stage 10)
- **ado-review** (`~/Dev/ado-review`) — Prototype, active across 5 repos

## Key Decisions
| Decision | Choice |
|---|---|
| Autonomy | Supervised |
| Pipeline | 4 phases, 17 stages, 3 parallel streams, AGILE sprints |
| Discovery | First-class, business need before code |
| Planning | BA + Architect + Design parallel, ALIGN sync point |
| Engineering | Sprint loop, task-level lifecycle, feedback loops |
| Delivery | Staging auto, prod human, continuous observe → intake |
| Design | Ingress or generated, same gate |
| Differentiator | Full-lifecycle AI, Discovery is the moat |
| Interface Architecture | Interface-agnostic orchestrator — CLI, REST API, OpenCode, JIRA, email all drive the same pipeline. Stages can pause and surface questions. Interface decides how to collect answers. |
| User Interaction | Stages return 'waiting-for-input' with questions. Orchestrator pauses. Interface collects answers. Runner.resume(runId, answers) continues. |
| OpenCode Role | Just one interface, not the workflow itself. Agent system prompts per stage (intake.md, evaluate.md) define role boundaries. |
| OpenCode | EPHEMERAL per task — opencode.json in workspace, `opencode run < prompt`, NO --attach, NO --agent flags on CLI |
| Ralph Wiggum | From user terminal: `ralph "<prompt>" -- --agent build` · From OpenCode session: `env -i ... ralph --prompt-file /tmp/task.md -- --agent build` |
| Project Mgmt | Redmine (GPL v2, truly free, self-hosted, MCP support) |
| Git Platform | Forgejo (community-governed, lightweight, PR support, MCP) |
| Design Platform | Penpot (open-source Figma alternative, official MCP server) |
| Chat / Comms | Zulip (Apache 2.0, topic-based threading, community MCP servers, agent↔human conversations) |
| Dogfooding | Use Redmine + Forgejo + Penpot + Zulip to manage TurboJET's own development |
| Target | Engineering teams |

## Tech Stack
- **Language**: TypeScript (ecosystem alignment with OpenCode SDK + AI dev tools)
- **Runtime**: Bun (single binary compile, faster than Node, built-in package manager)
- **Rationale**: OpenCode SDK is TS-native; AI dev CLI ecosystem is TS-based; if performance becomes critical, Rust for specific components
- **Zettelkasten**: `20260503T203946847206000`

## Architecture (2026-05-11)

### Three-Agent System: Chef → Manager → Stage

```
User ←→ Chef (OpenCode primary agent)
              ↕ (TurboJET plugin + custom tools — TypeScript code)
         Manager (pipeline state machine + SDK sessions)
              ↕ (OpenCode SDK sessions via promptAsync)
         Stage Agents (LLM with tools)
```

**Chef**: OpenCode primary agent. Talks to user, delegates to Manager.
**Manager**: Pipeline code. Manages stages, progress, retries. Returns structured JSON.
**Stage Agents**: One per stage. Specialized AI with tools. Returns structured JSON.

### Wake-Up Mechanism (VALIDATED)

Pipeline code calls `promptAsync` on Chef's OpenCode session to wake it up:
```
curl -X POST http://127.0.0.1:4096/session/{chefSessionId}/prompt_async \
  -d '{"parts":[{"type":"text","text":"[Manager] E8 completed. Result: ..."}]}'
```
- No user input needed — Chef LLM wakes up and processes the message
- Triggered deterministically by pipeline code when Manager needs Chef
- Tested and confirmed working (2026-05-11)

### Interactivity
- **D1, D2**: Interactive (questions to user, Chef mediates)
- **P1-P5**: Autonomous (Manager handles)
- **E6-E11**: Autonomous with retry loop (E9/E10/E11 → retry from E7)
- **E→D gate**: Requires Chef approval (configurable auto/manual)
- **D12-D15**: Autonomous

## Implementation Status (2026-05-12)

### Done
- All 18 stages implemented with AI agents (487 tests, 0 failures, typecheck clean)
- 18 agent prompts in `src/engine/agents/`
- 18 JSON schemas in `src/engine/schemas/`
- PipelineRunner with full 18-stage registry
- SprintController with retry loop
- AIClient with session management (createSession, continueSession, continueStructured)
- D2 Go/No-Go gate, P5 validation gate
- Beads backlog integration (P1 writes stories)
- CLI with interactive mode
- `promptAsync` wake-up mechanism validated
- **Chef/Manager/Stage Agent Integration (ff1 epic — COMPLETE)**
  - OpenCode plugin skeleton (turbojet.ts)
  - 5 custom tools (start, status, answer, approve, list)
  - Manager state machine (stage transitions, gates, retry logic, sprint loop)
  - StageAgentExecutor with execute() + continue() + executeAsync()
  - promptAsync wake-up module
  - Shared singleton context (turbojet-context.ts)
  - **E8 real code execution** — tool-enabled stages use `executeAsync` with `promptAsync`
  - Plugin session.idle handler for completion detection
- **D1→D2 PoC VALIDATED with real AI** — full Manager-mediated flow tested end-to-end
  - D1 Intake: raw user input → structured requirements + ambiguities (~130s)
  - D2 Evaluate: challenges assumptions, returns verdict with clarifying questions
  - D2 Interactive loop: same-session continuation with user answers, multi-round until `proceed`
  - Smoke test: `scripts/smoke-d1d2.ts`

### Validated But Not Yet Tested
- D2→P1+ artifact chain (only D1→D2 tested with real AI)
- Tool-enabled stages (E8/E9 via executeAsync)
- Plugin session.idle handler
- Wake-up mechanism in pipeline context
- Sprint loop (E6→E11 cycle)

### Next
- Extend smoke test to P1→P5 (planning phase with real AI)
- Test tool-enabled stages (E8 real code execution)
- End-to-end pipeline test: D1→D15 with a real feature request
- REST API endpoint
- External adapter integrations (Redmine, Forgejo, Zulip, Penpot)

## Open Threads
1. ~~Tech stack~~ — Resolved: TypeScript + Bun
2. ~~Project management~~ — Resolved: Redmine (dogfooding from day one)
3. ~~Git platform~~ — Resolved: Forgejo (dogfooding from day one)
4. ~~State management~~ — Resolved: SQLite + Events + Filesystem artifacts
5. ~~D1 Intake PoC~~ — Working with real AI via OpenCode SDK (2026-05-11)
6. Multi-modal ingestion (D1) — text-only PoC for v1
7. Conversation engine for P3 ALIGN
8. Sprint board / task state management
9. Monitoring integration for OBSERVE stage
10. Design generation capabilities (P2b)
11. How to evolve sdlc-flows into the full pipeline
12. Cost model — LLM costs across 17 stages per work item; smart model routing
13. Plugin architecture design — how stages/integrations register and execute
14. Redmine + Forgejo deployment for TurboJET development
