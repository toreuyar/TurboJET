# TurboJET — Architecture Specification v1

> SDLC Orchestration Service
> Status: Design Phase
> Date: 2026-05-13
> Language: Rust

---

## 1. Overview

TurboJET is a system-level service that orchestrates autonomous software development lifecycle (SDLC) pipelines. It provides:

- An **OpenAI-compatible REST API** for universal client connectivity
- A **deterministic pipeline runtime** (Manager) that executes state machine workflows
- A **pluggable runner system** that delegates code execution to any coding tool (OpenCode, Claude Code, Codex CLI, Gemini CLI, etc.)
- A **user-facing LLM agent** (Chef) that translates pipeline state into human conversation

TurboJET is not a coding tool. It is the orchestration brain that coordinates coding tools through structured pipelines.

---

## 2. Architecture

```
┌─────────────────────────────────────────────────────┐
│                    TurboJET (Rust)                   │
│                                                     │
│  ┌─────────────┐  ┌──────────────────────────────┐  │
│  │ Chef Agent  │  │        Manager (Core)         │  │
│  │ (LLM)       │  │  - Deterministic runtime      │  │
│  │             │  │  - Thread-safe                │  │
│  │ - Messenger │  │  - Multi-access safe          │  │
│  │ - No        │  │  - Resilient                  │  │
│  │   pipeline  │  │  - Resumable                  │  │
│  │   knowledge │  │                               │  │
│  └──────┬──────┘  │  ┌─────────┐  ┌───────────┐  │  │
│         │         │  │ SMG      │  │ State     │  │  │
│         │         │  │ Engine   │  │ Store     │  │  │
│         │         │  └─────────┘  └───────────┘  │  │
│         │         └──────────┬───────────────────┘  │
│         │                    │                      │
│         │         ┌──────────┴────────────┐         │
│         │         │    Runner Dispatch     │         │
│         │         │  ┌───────┐ ┌────────┐ │         │
│         │         │  │ LLM   │ │ Docker  │ │         │
│         │         │  │ Call  │ │ Runner  │ │         │
│         │         │  └───────┘ └────────┘ │         │
│         │         └───────────────────────┘         │
│         │                    │                      │
│  ┌──────┴───────┐    ┌──────┴──────────────┐        │
│  │ OpenAI-API   │    │ LLM Provider Router  │        │
│  │ Interface    │    │                      │        │
│  └──────────────┘    └─────────────────────┘        │
│                                                     │
└─────────────────────────────────────────────────────┘
         │                          │
    ┌────┴─────┐            ┌───────┴────────┐
    │  Users   │            │  LLM Providers │
    │ (any     │            │ (Anthropic,    │
    │  client) │            │  OpenAI, etc.) │
    └──────────┘            └────────────────┘
                                   │
                          ┌────────┴────────┐
                          │ Docker Container │
                          │ ┌─────────────┐  │
                          │ │ Coding Tool  │  │
                          │ │ (OpenCode,   │  │
                          │ │  Claude,     │  │
                          │ │  Codex, etc) │  │
                          │ └─────────────┘  │
                          └─────────────────┘
```

---

## 3. Core Components

### 3.1 Manager (Core Runtime)

The Manager is the main event loop of TurboJET. It is deterministic — no LLM involved.

**Properties:**
- **Thread-safe** — concurrent pipeline runs without data races
- **Multi-access safe** — multiple users interacting simultaneously
- **Resilient** — failures don't corrupt state, stages are retriable
- **Resumable** — state persists to disk, resumes after restart

**Responsibilities:**
1. Execute state machine graph (SMG) for each pipeline run
2. Dispatch stages to appropriate runners
3. Validate stage outputs against JSON schemas
4. Route to next stage per SMG rules
5. Manage gates — pause pipeline, notify Chef, await approval
6. Enforce hard limits (iteration per stage, steps per flow, depth globally)
7. Persist run state to durable storage
8. Handle concurrent runs with full isolation

**State Machine Execution:**
- Each pipeline run is an independent state machine instance
- State transitions are deterministic: SMG definition + output validation = next state
- Gate transitions require external input (user approval relayed via Chef)
- State is serializable and recoverable

**Concurrency Model:**
- Async runtime (tokio)
- Each pipeline run is an independent task
- Shared resources (LLM connections, Docker) managed through async primitives
- Rust's ownership model provides compile-time thread safety

### 3.2 Chef (User Messenger)

Chef is an LLM-based agent that serves as the user's interface to TurboJET.

**Role:** Messenger, not supervisor. Chef does NOT manage the pipeline.

**Responsibilities:**
1. Translate pipeline state changes into human-readable messages
2. Present gate approval requests to the user
3. Pass user confirmations back to Manager
4. Answer questions about pipeline status

**What Chef does NOT do:**
- Make pipeline routing decisions
- Understand pipeline internals
- Supervise stage execution
- Handle stage outputs

**Interaction Pattern:**
```
Manager → Chef: "P-stages completed, gate reached, awaiting approval for E-stages"
Chef → User:    "Planning is complete. Ready to start engineering? [plan summary]"
User → Chef:    "Looks good, proceed"
Chef → Manager: "User approved gate, proceed"
```

### 3.3 Runner Dispatch

Two execution paths based on stage type:

| Stage Type | Execution | Examples |
|-----------|-----------|---------|
| LLM-only | Direct LLM call within TurboJET | D1 (intake), D2 (evaluate), P3-P5 (planning) |
| Code | Docker container with coding tool | E6-E11 (engineering), D12-D15 (delivery) |

### 3.4 Runner Abstraction

Runners are treated as function calls:

```
input (JSON artifacts + prompt) → Runner → output (JSON artifacts)
```

**Runner Interface (conceptual):**
```rust
trait Runner {
    async fn execute(task: TaskDefinition, input: Artifacts) -> Result<Artifacts, RunnerError>;
}
```

**Docker Runner Model:**
- Each code stage runs in an isolated Docker container
- Container has full permissions inside its environment
- Host project data mounted at known path
- Coding tool auto-starts with pre-configured environment (docker-compose)
- Watchdog timer kills container on timeout
- Output written to mounted directory as structured JSON

**Pluggable Runners:**
- OpenCode (REST API or CLI mode)
- Claude Code (CLI)
- OpenAI Codex CLI (CLI)
- Gemini CLI (CLI)
- Any future tool that accepts prompts and produces output

**Multi-runner is a feature:** Different runners for different providers/stages. Use Claude Code for Anthropic models, Codex CLI for OpenAI models, etc.

### 3.5 OpenAI-Compatible API

TurboJET exposes a standard OpenAI-compatible REST API:

- `POST /v1/chat/completions` — main chat interface (SSE streaming)
- `GET /v1/models` — list available models
- Standard tool call format for structured interactions

**Why OpenAI-compatible:**
- De facto standard for LLM chat interfaces
- Massive ecosystem of existing clients
- No need to build custom client protocol
- Streaming SSE out of the box

### 3.6 LLM Provider Router

Connects TurboJET to multiple LLM providers:
- Routes Chef conversations
- Handles LLM-only stage execution
- Provider configuration at system level (not user-configurable)
- Supports multiple providers simultaneously

---

## 4. Pipeline Design

### 4.1 State Machine Graph (SMG)

Workflows are defined as JSON state machine graphs. The default pipeline:

**4 Flows, 18 Stages:**

```
Discovery:     D1 (intake) → D2 (evaluate) → [gate]
Planning:      P3 (architect) → P4 (tasks) → P5 (assign) → [gate]
Engineering:   E6 (analysis) → E7 (design) → E8 (implement) → E9 (test) → E10 (review) → E11 (acceptance) → [gate]
Delivery:      D12 (integrate) → D13 (release) → D14 (verify) → D15 (observe)
```

**Features:**
- Cyclic routing (e.g., D2 reject → D1 re-intake)
- Gates at flow boundaries requiring user approval
- Conditional routing based on stage output
- Hard limits:
  - Iteration: 3 per stage (retry limit)
  - Steps: 20 per flow (prevents infinite loops)
  - Depth: 5 globally (prevents runaway recursion)

### 4.2 Agent Definition Configs (ADC)

Per-stage agent configuration in JSON:

```json
{
  "id": "e8-implement",
  "name": "Implement",
  "flow": "engineering",
  "promptPath": "prompts/engineering/e8-implement.md",
  "tools": ["bash", "read", "write", "edit", "glob", "grep"],
  "disabledTools": ["question", "webfetch"],
  "timeout": 3600,
  "permissions": { ... },
  "inputArtifacts": [
    { "name": "design", "schema": "schemas/engineering/e7-design.json" }
  ],
  "outputArtifacts": [
    { "name": "implementation", "schema": "schemas/engineering/e8-implementation.json" }
  ]
}
```

### 4.3 Artifact Passing

Filesystem-first approach:
1. Stage completes → writes output artifacts to directory as structured JSON
2. Manager validates output against schema
3. Next stage receives validated artifacts as input
4. Artifacts are the sole interface between stages

---

## 5. Data Flow

### 5.1 User Initiates Pipeline

```
User (chat client) → POST /v1/chat/completions → TurboJET
  → Chef receives message
  → Chef interprets as "start new pipeline"
  → Chef tells Manager: "new pipeline, description: ..."
  → Manager creates run, starts D1 (LLM call)
  → D1 completes, output validated
  → Manager routes to D2 (LLM call)
  → D2 completes, gate reached
  → Manager tells Chef: "discovery complete, gate reached"
  → Chef tells user: "Discovery done. [summary]. Proceed to planning?"
```

### 5.2 Stage Execution (Code Stage)

```
Manager → dispatch to Docker Runner
  → Pull/create container (pre-configured image)
  → Mount project directory + artifact input directory
  → Inject prompt + input artifacts
  → Start coding tool with task
  → Watchdog timer starts
  → Container executes...
  → Tool writes output artifacts to mounted directory
  → Container exits
  → Manager reads output artifacts
  → Manager validates against schema
  → If valid: route to next stage per SMG
  → If invalid: retry (up to iteration limit)
```

### 5.3 Gate Approval

```
Manager: gate reached, pipeline paused
  → Manager notifies Chef
  → Chef presents to user: "Planning complete. [plan summary]. Approve engineering?"
  → User responds: "Approved" or "Rejected" or "Modify X first"
  → Chef relays to Manager
  → Manager: if approved, resume pipeline; if rejected, cancel or reroute per SMG
```

---

## 6. User & Access Model

### 6.1 Multi-Role Access

TurboJET supports different user roles with different access levels:

| Role | Access |
|------|--------|
| Admin | Full pipeline control, system configuration, all runs |
| Developer | Start pipelines, approve gates, view own runs |
| Viewer | View pipeline status and logs, no write access |

### 6.2 Locked-In System

- Users cannot alter agents, providers, or system settings
- No access to internal configuration
- Controlled, predictable experience
- Thin client: chat in, responses out

### 6.3 Per-User Isolation

- Each user has independent pipeline runs
- No cross-user data leakage
- Concurrent runs are isolated

---

## 7. Resilience & Reliability

### 7.1 State Persistence

- Pipeline state persisted to durable storage (SQLite or filesystem)
- State includes: current stage, completed stages, artifacts, gate status
- On restart: Manager loads persisted state, resumes pending runs
- In-flight stages are re-dispatched or marked for retry

### 7.2 Failure Handling

| Failure | Handling |
|---------|----------|
| Stage output invalid | Retry up to iteration limit (3) |
| Runner timeout | Watchdog kills container, retry with fresh container |
| LLM provider failure | Fallback to alternate provider or retry |
| Manager crash | State persisted, resume on restart |
| Docker failure | Retry with new container, log error |

### 7.3 Hard Limits

- **Iteration limit** (3 per stage): prevents infinite retry loops
- **Step limit** (20 per flow): prevents runaway execution within a flow
- **Depth limit** (5 global): prevents runaway pipeline recursion
- **Timeout** (per stage, configurable via ADC): prevents hung stages

---

## 8. Technology Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust |
| Async Runtime | Tokio |
| HTTP Framework | Axum |
| Serialization | Serde (JSON) |
| State Storage | SQLite (via rusqlite) or sled |
| LLM Communication | reqwest + SSE |
| Container Management | Docker API (via bollard) |
| Logging | tracing + tracing-subscriber |
| Configuration | TOML or JSON |

---

## 9. Project Structure (Proposed)

```
turbojet/
  Cargo.toml
  src/
    main.rs                    # Entry point, server startup
    config.rs                  # System configuration loading
    api/                       # OpenAI-compatible API
      mod.rs
      chat.rs                  # /v1/chat/completions handler
      models.rs                # /v1/models handler
      streaming.rs             # SSE streaming
    manager/                   # Core runtime
      mod.rs
      state_machine.rs         # SMG execution engine
      run.rs                   # Pipeline run management
      gate.rs                  # Gate/approval management
      scheduler.rs             # Stage scheduling & dispatch
    runner/                    # Runner abstraction
      mod.rs
      trait.rs                 # Runner trait definition
      llm_runner.rs            # Direct LLM call runner
      docker_runner.rs         # Docker container runner
    chef/                      # Chef agent
      mod.rs
      agent.rs                 # LLM agent loop
      context.rs               # Pipeline state context for Chef
    provider/                  # LLM provider routing
      mod.rs
      router.rs                # Provider selection logic
      anthropic.rs             # Anthropic API client
      openai.rs                # OpenAI API client
    smg/                       # SMG loading & validation
      mod.rs
      loader.rs                # Load SMG from JSON
      types.rs                 # SMG Rust types
    adc/                       # ADC loading & validation
      mod.rs
      loader.rs                # Load ADC from JSON
      types.rs                 # ADC Rust types
    artifact/                  # Artifact validation
      mod.rs
      validator.rs             # JSON schema validation
    storage/                   # State persistence
      mod.rs
      store.rs                 # State store trait
      sqlite.rs                # SQLite implementation
  config/
    default.smg.json           # Default pipeline definition
    agents/                    # ADC files (18)
    prompts/                   # Agent prompts (18)
    schemas/                   # Output schemas (18)
    permissions/               # Permission rules
  docker/
    docker-compose.yml         # Runner container definitions
    opencode/
      Dockerfile
    claude-code/
      Dockerfile
    codex/
      Dockerfile
  tests/
    integration/
    fixtures/
```

---

## 10. Scope & Priorities

### Phase 1 — Core Runtime
- Manager state machine engine
- SMG loading and validation
- ADC loading and validation
- Artifact validation
- State persistence (resumability)
- Concurrent run management

### Phase 2 — API & Chef
- OpenAI-compatible REST API
- SSE streaming
- Chef agent loop
- LLM provider routing

### Phase 3 — Runners
- LLM-only runner (direct call)
- Docker runner (container management)
- OpenCode runner implementation
- Watchdog timer

### Phase 4 — Production
- Authentication & authorization
- Multi-role access control
- Monitoring & metrics
- Logging & observability
- Additional runner implementations (Claude Code, Codex, etc.)

---

## 11. Design Decisions & Rationale

| Decision | Rationale |
|----------|-----------|
| Rust over TypeScript | Single binary, no runtime, thread-safe by default, performance |
| Deterministic Manager | Predictable, testable, no LLM hallucination in orchestration |
| OpenAI-compatible API | Universal client compatibility, massive ecosystem |
| Docker for code stages | Isolation, disposability, pluggable tools |
| LLM-only for reasoning stages | No container overhead for stages that don't need code execution |
| Filesystem artifact passing | Simple, debuggable, no coupling between stages |
| Locked-in system | Controlled experience, no user misconfiguration |
| Chef as messenger only | Decoupled from pipeline logic, any LLM can do it |
| Multi-runner support | Use best tool for each provider, not locked to one |

---

## 12. Lessons from TypeScript Prototype

The TypeScript prototype (800 lines, 33 tests) validated:

1. **SMG schema design works** — cyclic routing, gates, limits all functional
2. **ADC permissions model works** — 3-tier resolution (inline → file ref → default)
3. **Stage isolation works** — clear input/output boundaries
4. **Filesystem artifact passing works** — simple and debuggable
5. **Three-agent split is sound** — Chef/Manager/Stage

The prototype failed on:
1. **OpenCode tool integration** — import coupling makes deployment impossible
2. **Plugin system** — unreliable context, singleton issues
3. **Engine-as-library** — can't deploy tools without engine source tree

These failures directly led to the standalone service architecture.

---

## Appendix A: SMG Schema (Reference)

From the TypeScript prototype — the default pipeline has:

- **Discovery flow**: D1 (intake), D2 (evaluate) with gate
- **Planning flow**: P3 (architect), P4 (tasks), P5 (assign) with gate
- **Engineering flow**: E6-E11 (analysis → design → implement → test → review → acceptance) with gate
- **Delivery flow**: D12-D15 (integrate → release → verify → observe)

Routing rules:
- D2 verdict "proceed" → P3
- D2 verdict "reject" → cancel
- D2 verdict "needs-clarification" → D1 re-intake
- Gate approval → next flow
- Gate rejection → cancel or reroute

---

## Appendix B: ADC Schema (Reference)

```json
{
  "id": "string",
  "name": "string",
  "flow": "string",
  "promptPath": "string",
  "tools": ["string"],
  "disabledTools": ["string"],
  "timeout": "number (seconds)",
  "permissions": {
    "rules": [
      {
        "tool": "string",
        "pattern": "string",
        "action": "allow | deny"
      }
    ]
  },
  "inputArtifacts": [
    { "name": "string", "schema": "string" }
  ],
  "outputArtifacts": [
    { "name": "string", "schema": "string" }
  ]
}
```
