# TurboJET — Architecture Specification v2

> SDLC Orchestration Service
> Status: **Implemented** — v2 architecture compiles and runs end-to-end
> Date: 2026-05-13
> Language: Rust

---

## 1. Overview

TurboJET is a system-level service that orchestrates autonomous software development lifecycle (SDLC) pipelines. It is built in Rust, exposes an OpenAI-compatible API, and uses pluggable coding tools for stage execution.

---

## 2. Four-Layer Architecture

```
┌─────────────────────────────────────────┐
│           Manager (ASYNC)                │
│  Event loop, orchestration, watchdog     │
│  Receives: user input, approvals, events │
│  Monitors sync ops via DB                │
└──────────────┬──────────────────────────┘
               │ spawns
┌──────────────▼──────────────────────────┐
│           Flow (SYNC)                    │
│  Runs stages sequentially within a flow  │
│  Writes state to DB                      │
│  Returns: completed | gate | failed      │
└──────────────┬──────────────────────────┘
               │ spawns
┌──────────────▼──────────────────────────┐
│           Stage (SYNC)                   │
│  Single stage execution                  │
│  Loads ADC, builds prompt, dispatches    │
│  Writes state + artifacts to DB          │
└──────────────┬──────────────────────────┘
               │ dispatches
┌──────────────▼──────────────────────────┐
│           Tool (execution primitive)     │
│  LLM call or Docker container            │
│  Receives prompt, returns raw output     │
└─────────────────────────────────────────┘
```

---

## 3. Execution Model

### 3.1 Manager (ASYNC — the only async component)

- Owns the event loop (tokio runtime)
- Receives events: user messages, gate approvals, cancellations, watchdog alerts
- Spawns Flow operations (sync, blocking)
- Monitors sync operations by reading DB state
- Watchdog: periodically checks DB for stale Flow/Stage records
- Never executes anything directly — only coordinates
- Resumable: reads DB on startup, continues from last known state

**Manager is NOT:**
- A state machine engine
- A prompt builder
- An ADC loader
- An LLM caller

**Manager IS:**
- An event processor
- A Flow spawner
- A watchdog
- A state observer via DB

### 3.2 Flow (SYNC — tracked in DB)

- Blocks from first stage to last stage (or gate)
- Runs stages sequentially
- Handles internal stage routing (output of stage A determines stage B)
- Records state to DB: current stage, completed stages, status
- Returns to Manager when:
  - Flow completes all stages → Manager routes to next flow
  - Gate reached → Manager asks user for approval
  - Stage fails after retries → Manager handles failure

**Flow has:**
- ID (UUID, stored in DB)
- Parent pipeline run ID
- Flow name (discovery, planning, engineering, delivery)
- Status: pending, running, completed, failed
- Current stage ID
- List of completed stage IDs
- Timestamps: created_at, started_at, completed_at

### 3.3 Stage (SYNC — tracked in DB)

- Blocks from start to finish
- Single responsibility: execute one stage
- Loads its own ADC
- Builds its own prompt from ADC + input artifacts
- Dispatches to the appropriate Tool
- Collects output, writes artifacts to DB
- Validates output against schema (if defined in ADC)

**Stage has:**
- ID (UUID, stored in DB)
- Parent flow ID
- Stage name (d1-intake, e8-implement, etc.)
- Status: pending, running, completed, failed, timed_out
- Input artifacts (JSON)
- Output artifacts (JSON)
- Timestamps: created_at, started_at, completed_at

### 3.4 Tool (execution primitive)

Two types:
- **LLM Tool**: direct LLM API call (for reasoning stages: D1, D2, P3-P5)
- **Docker Tool**: container execution (for code stages: E6-E11, D12-D15)

Tool receives: prompt + input artifacts
Tool returns: raw output (text or file artifacts)

---

## 4. Sync/Async Boundary

The DB is the bridge between sync and async worlds:

```
ASYNC world                    SYNC world
┌──────────┐                   ┌──────────┐
│ Manager  │ ──spawns────────→ │  Flow    │
│ (event   │                   │  Stage   │
│  loop)   │ ←──reads DB────── │  Tool    │
│          │                   └──────────┘
│ watchdog │ ──checks DB for stale records──→ kills if needed
└──────────┘
```

**Why this matters:**
- Manager can observe sync operations without blocking
- DB is the single source of truth
- Deterministic: at any point, DB reflects exact state
- Resumable: restart Manager, it reads DB, continues
- Observable: any client can query API, API reads DB

---

## 5. DB Schema

### pipeline_runs
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (UUID) | Primary key |
| description | TEXT | User's original description |
| status | TEXT | running, waiting_for_gate, completed, failed, cancelled |
| current_flow_id | TEXT (FK) | Currently active flow |
| gate_state | TEXT (JSON) | Current gate info if waiting |
| created_at | TEXT (ISO 8601) | |
| updated_at | TEXT (ISO 8601) | |

### flows
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (UUID) | Primary key |
| run_id | TEXT (FK) | Parent pipeline run |
| flow_name | TEXT | discovery, planning, engineering, delivery |
| status | TEXT | pending, running, completed, failed |
| current_stage_id | TEXT (FK) | Currently executing stage |
| created_at | TEXT (ISO 8601) | |
| started_at | TEXT (ISO 8601) | |
| completed_at | TEXT (ISO 8601) | |

### stages
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (UUID) | Primary key |
| flow_id | TEXT (FK) | Parent flow |
| stage_name | TEXT | d1-intake, e8-implement, etc. |
| status | TEXT | pending, running, completed, failed, timed_out |
| input_artifacts | TEXT (JSON) | Input data |
| output_artifacts | TEXT (JSON) | Output data |
| error | TEXT | Error message if failed |
| iteration | INTEGER | Current retry count |
| created_at | TEXT (ISO 8601) | |
| started_at | TEXT (ISO 8601) | |
| completed_at | TEXT (ISO 8601) | |

---

## 6. Event Flow

### 6.1 User starts a pipeline

```
User → POST /v1/chat/completions → API
  → API sends event to Manager
  → Manager creates PipelineRun in DB
  → Manager creates Flow(discovery) in DB
  → Manager spawns Flow.run() (sync, blocking thread)
    → Flow creates Stage(d1-intake) in DB
    → Flow calls Stage.run() (sync)
      → Stage loads ADC, builds prompt, calls Tool
      → Stage writes result to DB
    → Flow reads result, routes to next stage (d2-evaluate)
    → Flow creates Stage(d2-evaluate) in DB
    → Flow calls Stage.run()
      → Stage runs...
    → d2-evaluate output: verdict=proceed, gate=true
    → Flow returns: GateReached
  → Manager reads DB, sees gate
  → Manager notifies Chef → User: "Discovery done. Approve?"
```

### 6.2 User approves gate

```
User → POST /v1/pipeline/runs/:id/approve → API
  → API sends event to Manager
  → Manager updates PipelineRun: gate approved
  → Manager creates Flow(planning) in DB
  → Manager spawns Flow.run()
    → ... stages run ...
  → Flow returns: GateReached
  → Manager notifies Chef → User: "Planning done. Approve?"
```

### 6.3 Watchdog kills stuck stage

```
Manager watchdog timer fires
  → Manager reads DB: stages WHERE status='running' AND updated_at < now() - timeout
  → Found: stage e8-implement, running for 2 hours
  → Manager kills the sync operation
  → Manager updates DB: stage status = timed_out
  → Manager marks Flow as failed
  → Manager marks PipelineRun as failed
  → Manager notifies Chef → User: "Pipeline failed: e8-implement timed out"
```

---

## 7. Crates (Revised)

```
turbojet/
  crates/
    tj-config/      # Types, loaders, validation (SMG, ADC, schemas)
    tj-db/          # SQLite schema, queries, migration
    tj-manager/     # Async event loop, watchdog, Flow spawning
    tj-flow/        # Sync: stage routing within a flow
    tj-stage/       # Sync: single stage execution (ADC, prompt, dispatch)
    tj-tool/        # LLM call, Docker container execution
    tj-chef/        # User-facing LLM agent (messenger)
    tj-api/         # Axum HTTP server, thin event translator
  config/
  docker/
```

---

## 8. Dependency Graph

```
tj-api
  ├── tj-manager
  │     ├── tj-db
  │     ├── tj-flow
  │     │     ├── tj-stage
  │     │     │     ├── tj-tool
  │     │     │     │     └── tj-config
  │     │     │     └── tj-config
  │     │     └── tj-db
  │     └── tj-config
  ├── tj-chef
  │     ├── tj-tool
  │     └── tj-config
  └── tj-config
```

Each layer only depends on the layer below it. No circular deps.

---

## 9. Notification Chain (Hierarchical Delegation)

Each sync object wakes its direct parent. Never skips levels.

```
Tool finishes     → Stage picks up result
Stage finishes    → Flow picks up result, routes to next Stage
Flow finishes     → Manager picks up result, routes to next Flow
Flow hits gate    → Manager asks user for approval
Watchdog fires    → Manager checks specific object in DB
User input        → Manager processes
External trigger  → Manager processes
```

**Manager never hears about individual stages.** Flow handles all internal stage routing. Manager only knows: Flow completed, Flow hit gate, Flow failed.

**Manager is an actor, not a loop:**
- Sleeps until event arrives on its channel
- Events wake it up: FlowDone, FlowGate, WatchdogTick, UserInput, ExternalTrigger
- Wakes up, reads DB, decides, acts, goes back to sleep
- No busy polling. No cron. Pure event-driven.
- Can be destroyed between events — all state in DB

**Multiple concurrent objects:**
- Multiple Flows and Stages can run simultaneously
- Each in its own thread with its own git worktree/Docker container
- Each has its own watchdog timer
- Manager tracks all via DB + per-object timers

**Notification mechanism:**
- Stage → Flow: thread join handle or sync channel (sync → sync)
- Flow → Manager: send event on async channel (sync → async)
- Watchdog → Manager: timer fires event on channel (async → async)

---

## 10. Manager Event Types

```rust
enum ManagerEvent {
    StartPipeline { description: String },
    GateApproved { run_id: String },
    GateRejected { run_id: String, reason: String },
    CancelPipeline { run_id: String, reason: String },
    FlowCompleted { run_id: String, flow_id: String },
    FlowFailed { run_id: String, flow_id: String, reason: String },
    WatchdogTick,
    Shutdown,
}
```

Manager receives events via a channel (`tokio::sync::mpsc`). API handlers send events. Manager processes them sequentially.

---

## 11. Key Design Principles

1. **Only Manager is async** — everything else is sync
2. **DB is the bridge** — sync operations write state, async reads it
3. **Deterministic tracking** — DB always reflects exact state
4. **Resumable** — restart Manager, reads DB, continues
5. **Watchdog** — Manager periodically checks for stale operations
6. **Four layers** — Manager → Flow → Stage → Tool, each talks one level down
7. **Flow and Stage identified in DB** — every operation has an ID, traceable end-to-end
