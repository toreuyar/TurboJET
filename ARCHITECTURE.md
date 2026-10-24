# TurboJET Architecture

> **Current-state note (2026-10-22):** this document retains the July 2026 architecture narrative and historical snapshot. For current evidence and limitations use [CAPABILITIES.md](CAPABILITIES.md) and [docs/checks.md](docs/checks.md). In particular, the runtime is asynchronous, runs use immutable execution bundles and durable budgets, questions/answers and interrupted-run recovery are durable, gate routing/completions have durable effects, model-driven shell execution has a fail-closed local TJ-034 Landlock/seccomp boundary, and the worktree is still **not** a sandbox. Real-forge SSH/PR publication and a real Engineering pilot remain BLOCKED.

This document describes committed source at repository snapshot `master@2cf96d4` plus explicitly identified uncommitted documentation and evidence reviewed on 2026-07-20. This document and those uncommitted materials are not necessarily part of the committed snapshot. It is a current-state reference, not a product promise or test report. See [VISION.md](VISION.md) for intended direction and [CAPABILITIES.md](CAPABILITIES.md) for evidence maturity.

## System Context

TurboJET is a Rust service that accepts chat and pipeline requests, persists orchestration state in SQLite, executes configured stages through in-process ADK model calls and native tools, and can communicate through Zulip and Forgejo.

```text
HTTP client / Zulip user
          |
          v
   tj-api / tj-agent
          |
          v
 ManagerActor (async actor, lifecycle owner)
          |
          v
      Flow (async SMG routing)
          |
          v
      Stage (async ADC/provider/session handling)
          |
          v
    ADKRunner (async in-process model/tool loop)
          |
          v
 tj-tools: read / write / edit / bash

SQLite <---- Manager, Flow, Stage, secrets
Zulip <---- Concierge, flow sessions, notifications
Git/worktree/Forgejo <---- Manager lifecycle
```

The execution chain is asynchronous from Manager through Flow, Stage, and runner. SQLite access through `tj-db` is synchronous. Manager paths move potentially blocking git operations to `tokio::task::spawn_blocking`; this does not make the database itself asynchronous (`crates/tj-manager/src/actor.rs:941-962`, `crates/tj-manager/src/actor.rs:1551-1565`, `crates/tj-db/src/lib.rs:685-820`).

There is no Docker or OpenCode execution path in the current runtime. ADK model calls and tools execute in the TurboJET process. A git worktree separates branches and organizes task files; it is not a process, filesystem, network, credential, or privilege boundary. Stale non-runtime OpenCode residue remains in `docker/Dockerfile`; none of it is the current runtime.

## Component Model

### API and Concierge

`tj-api` builds an Axum router for health, OpenAI-compatible chat/model endpoints, pipeline control, SSE events, logs, secrets, and SSH keys. `AppState` receives a `SpawnResult` containing the public `ManagerHandle` and a separate optional secrets store. The store is not placed on `ManagerHandle`, so the `SystemOperations` interface used by the LLM-facing Concierge has no direct API method to retrieve secret values (`crates/tj-manager/src/actor.rs:357-360`, `crates/tj-api/src/handlers.rs:175-184`). This separation is not an LLM security boundary: LLM-invoked Bash inherits the TurboJET process environment and has host filesystem access.

`tj-agent` implements the user-facing Agent and Concierge. Both call a `SystemOperations` abstraction implemented by `ManagerHandle`. Concierge model calls use `tj-provider`; stage model calls use ADK through `tj-stage` and `tj-runner`, so these are separate provider paths.

### Manager

`ManagerActor` owns the event loop, run lifecycle, flow dispatch, gates, watchdog timers, pipeline notifications, task worktrees, per-project git-provider cache, secrets-backed git setup, and PR lifecycle. Flows run as detached Tokio tasks and return `ManagerEvent::FlowDone` events. Panic capture converts a flow panic into a failed outcome (`crates/tj-manager/src/actor.rs:1648-1696`).

`ManagerHandle` is the API/Concierge-facing handle. `SpawnResult` keeps `ManagerHandle` and `secrets_store` separate. `ManagerActor` and `AppState` may hold the same `Arc<dyn SecretsStore>`, while LLM-facing `SystemOperations` has no direct secret-retrieval method. That API separation does not contain Bash or prevent access through inherited environment variables or the host filesystem.

### Flow

`tj-flow` executes one SMG flow. It creates durable flow and stage records, selects stages, awaits `Stage::execute`, routes on the completed stage's validated artifact `verdict`, and enforces bounded steps/iterations. Routing is verdict-first and fail-closed: a stage cannot reach routing without a completed status and a `verdict` matching a declared route key, and a missing or unroutable verdict fails the flow with a specific, logged error instead of defaulting (`crates/tj-flow/src/flow.rs:145-188`, `crates/tj-flow/src/flow.rs:608-634`). Flow receives an optional workspace path and branch from Manager; it does not own worktree creation or cleanup.

### Stage

`tj-stage` resolves the stage definition, ADC, prompt, provider, model, workspace, artifacts, timeout, and chat session. It constructs an `ADKRunner`, maps runner results to durable stage status, and handles interactive follow-up through a chat session. For interactive stages it wires the runner's blocking `question` channel over that session and durably flips the stage record `Running → WaitingForInput → Running` around each blocking ask, with a resurrect guard that refuses to overwrite a stage row that went terminal mid-ask (`crates/tj-stage/src/question_channel.rs`).

Stage construction no longer falls back to guessed ADC/prompt defaults: admission freezes the ADC, prompt, schema, provider and model in the execution bundle, and missing/invalid references fail before a run exists. Interactive and non-interactive completion validate structured output against the frozen schema and verify required output artifacts locally (`crates/tj-stage/src/executor.rs`, `crates/tj-config/src/bundle.rs`). The supported schema vocabulary is deliberately limited, not full JSON Schema.

### Runner and Tools

`tj-runner` owns `ADKRunner`, per-stage in-memory sessions, the bounded model/tool loop, tool-based completion, and heuristic JSON extraction. It calls `adk_core::Llm::generate_content` in-process. Interactive stages complete only through a `verdict` tool call: its arguments are validated before success is reported (JSON object, mandatory `verdict` key, and the stage's declared schema when present), invalid arguments are poked back to the same model in-session bounded by the ADC `max_output_retries`, and exhaustion fails the stage visibly with an `output contract violation:` reason — never a silent pass and never a `WaitingForInput` degradation. A turn that ends without a tool call never completes an interactive stage; it is nudged back with the same bound (`crates/tj-runner/src/adk_runner.rs:20`, `crates/tj-runner/src/adk_runner.rs:784-880`). User questions travel through a blocking `question` tool inside the same conversation. The legacy `::TURBOJET:DONE::` text marker is retired; its constant remains declared but no completion path reads it. Non-interactive stages keep the tool loop (`MAX_TOOL_ITERATIONS=20`): final text is extracted from model output and retried for parseable JSON (`extract_json` plus a bounded corrective retry), with the declared schema shaping the model request while verdict presence is enforced at flow routing (`crates/tj-runner/src/adk_runner.rs:1200-1245`). Timeout semantics differ by channel: interactive loops bound each LLM call so a blocking ask is not charged against the stage, while non-interactive loops keep a whole-loop deadline (`crates/tj-runner/src/adk_runner.rs:1133-1160`).

`tj-tools` provides `read_file`, `write_file`, `edit_file`, and `bash`. File tools use a canonical workspace-root validator. `EditTool` applies a fuzzy replacement cascade. `ShellTool` executes `bash -c` under the TJ-034 boundary: it refuses to run unless Landlock (filesystem + network) and seccomp restrictions plus finite CPU/memory/file/process limits are installed, uses a fixed environment allow-list rather than inheriting credentials, and writes only inside the canonical workspace/output roots. Shell commands also run in a process group; timeout, cancellation, and normal exit join ordinary descendants. This is a narrow local Linux boundary, not a container or a promise of safety for hostile native code (`crates/tj-tools/src/isolation.rs`, `crates/tj-tools/src/shell.rs`).

## Crates and Dependencies

The workspace has 15 crates (`Cargo.toml:3-19`). The dependency column lists direct TurboJET-internal dependencies from crate manifests; third-party dependencies are omitted except where architecturally important.

| Crate | Current responsibility | Direct internal dependencies |
|---|---|---|
| `tj-api` | Axum server, HTTP/SSE routes, request logging, API sessions, secrets and SSH endpoints | `tj-config`, `tj-manager`, `tj-db`, `tj-agent`, `tj-provider`, `tj-runner`, `tj-logs`, `tj-secrets` |
| `tj-manager` | Async actor, run/flow lifecycle, gates, watchdogs, worktrees, git providers, notifications, secret-backed SSH use | `tj-config`, `tj-db`, `tj-stage`, `tj-flow`, `tj-chat`, `tj-chat-zulip`, `tj-agent`, `tj-provider`, `tj-git`, `tj-secrets` |
| `tj-flow` | SMG flow and stage routing, bounded execution, flow/stage persistence | `tj-config`, `tj-db`, `tj-stage`, `tj-chat` |
| `tj-stage` | ADC/prompt resolution, ADK provider construction, runner invocation, interactive stage handling | `tj-chat`, `tj-config`, `tj-db`, `tj-runner`; also `adk-core`, `adk-model` |
| `tj-runner` | In-process ADK session/model/tool loop and structured-text extraction | `tj-config`, `tj-db`, `tj-tools`; also `adk-core`, `adk-model` |
| `tj-tools` | Native read/write/edit/bash tools, path validation, fuzzy replacement | None |
| `tj-config` | System config, SMG, ADC, and prompt loading/types | None |
| `tj-db` | Synchronous SQLite schema, migrations, records, and queries | None |
| `tj-agent` | Agent, Concierge, sessions, LLM-call tool registry, `SystemOperations` boundary | `tj-config`, `tj-provider`, `tj-chat` |
| `tj-provider` | Concierge-facing OpenAI-compatible and Anthropic provider clients/types | `tj-config` |
| `tj-chat` | Chat provider/session abstractions and conversation types | None |
| `tj-chat-zulip` | Zulip chat provider, long polling, DM/flow sessions | `tj-chat` |
| `tj-git` | Worktree commands, branch naming, git-provider abstraction, Forgejo implementation | `tj-secrets` |
| `tj-secrets` | Async, object-safe per-project secret-store trait | None |
| `tj-logs` | JSONL log types, filtering, reading, and tracing-layer support | None |

The stage provider factory has constructors for OpenAI, Anthropic, Gemini, and custom OpenAI-compatible providers. Only the configured Z.AI-compatible path has meaningful repository evidence; the provider matrix is not operationally verified. Workspace ADK Git dependencies are pinned with a `rev` and use `--locked --offline` checks (`Cargo.toml`).

## Workflow Configuration

### SMG

The State Machine Graph defines flow order, flow/gate type, entry/exit stages, stage routes, and global limits. There is no built-in or default graph. Shipped workflows are self-contained packages under `workflows/` (Discovery ends at `discovery-gate`; the three generic packages are repository-free), and the full SDLC package is an operator-local opt-in. Delivery, when a workflow declares it, is an external CI/CD integration triggered explicitly after publication, not a model-executed flow. Manager owns cross-flow sequencing and gates; Flow owns routing within a flow.

Reserved route targets include `_next-flow`, `_end`, and `_exit`, which complete the flow, and `_escalate`, which fails the flow loudly as requiring human intervention (`crates/tj-flow/src/flow.rs:779-800`). Routing is fail-closed and verdict-first: the completed stage's artifact must carry a `verdict` string matching one of the stage's declared route keys; a missing artifact, a missing verdict, or an unknown verdict value fails the flow with a specific error naming the stage — there is no default verdict and no implicit fallback route (`crates/tj-flow/src/flow.rs:145-188`, `crates/tj-flow/src/flow.rs:608-634`). In the shipped Discovery package evaluate's verdict space is `proceed | reject` and packet completes with `generated → _next-flow` (`workflows/discovery/workflow.smg.json`); the local full-SDLC package uses `approved | rejected` for validate and `success → _exit` for observe. SMG/ADC/limits types deny unknown fields. Legacy `merge_strategy`/`pr_config` metadata is recognized narrowly as inert typed fields rather than silently accepting arbitrary keys (`crates/tj-config/src/smg.rs`).

### ADC

An Agent Definition Config identifies the prompt, provider/model overrides, input/output artifact declarations, enabled tools, timeout, and `max_output_retries` (the shared poke/nudge bound, default 3). For example, Discovery's intake declares a required non-empty `output.md`, read/write tools, and a 300-second timeout (`workflows/discovery/agents/intake/agent.json`). Package resources resolve only inside their own package; legacy `file`/`directory` graphs may still use the optional `adc_dir` for bare agent ids. These declarations guide prompts, tool registration, and artifact handling. A declared `schema` is now enforced on the completion path: interactive `verdict` tool arguments are validated against it (required keys, types, string enums) before the stage can complete, and config lint tests keep every SMG route key covered by the agent's declared verdict vocabulary. The validator supports only `required`/`properties`/`type`/`enum` constraints, not full JSON Schema (`crates/tj-config/src/loader.rs:173-204`).

## Lifecycle, State, and Events

The durable hierarchy is Project -> Lane -> Pipeline Run -> Flow -> Stage, plus immutable execution bundles, durable questions, run budgets, pending effects and recovery rows. SQLite access is synchronous, but lifecycle transitions, gate decisions, question answers, budget charges and effect creation use short `BEGIN IMMEDIATE` transactions with revision compare-and-set and audit/event rows (`crates/tj-db/src/lib.rs`, `crates/tj-db/src/lifecycle.rs`). External Git/forge/chat effects remain outside the database transaction.

Principal statuses are:

| Record | Statuses |
|---|---|
| Run | `Running`, `WaitingForGate`, `Interrupted`, `Completed`, `Failed`, `Cancelled` |
| Flow | `Pending`, `Running`, `Completed`, `Failed`, `TimedOut` |
| Stage | `Pending`, `Running`, `WaitingForInput`, `Completed`, `Failed`, `TimedOut` |

Principal Manager events include run request, flow completion, gate decision, cancellation, watchdog tick, stage input, secret update, and a legacy merge-complete route. Their behavior is not uniform: `UserInput` only logs, direct `send_stage_message` is a no-op, and interactive input instead travels through Flow-owned chat sessions. The legacy merge-complete route exists but is also a no-op; it is not a mutating merge operation. Pipeline notifications are broadcast to API sessions and chat-facing components.

A typical run is:

1. API or Concierge creates a durable run and sends `RunRequested`.
2. Manager resolves a project workspace/repository and creates or reconstructs one task worktree when applicable.
3. Manager spawns the current SMG flow and starts a watchdog.
4. Flow creates/updates durable flow and stage records while awaiting Stage and ADKRunner.
5. Interactive stages ask users through a blocking `question` tool over the `ChatSession` — the stage record flips `Running → WaitingForInput → Running` around each ask — and complete only via a validated `verdict` tool call; non-interactive stages return parsed JSON output whose `verdict` key drives routing.
6. A completed flow returns artifacts to Manager. Only an Engineering flow released by an exact approved task selection may commit/push/open one draft PR; a missing/failed test/review/acceptance verdict, out-of-scope change or failed remote operation fails the run instead of claiming publication. Manager then routes to the next flow or gate.
7. TurboJET never marks a PR ready or merges; human review/merge remains the authority boundary. Completion may clean up a clean, fully published worktree, while unpublished or uncommitted work is retained.

`FlowOutcome::WaitingForInput` is dormant: interactive waits happen inside the stage behind durable `WaitingForInput` status flips around each blocking `question` ask, and no production path produces the outer outcome. If it ever reached Manager, it is treated as a visible failure rather than routed onward as success (`crates/tj-manager/src/actor.rs`).

## Workspace, Git, Forgejo, and SSH

Manager owns one task-scoped worktree and branch, normally `tj/{run_id}-{slug}`, across all flows. Flow receives the workspace path. Worktree creation, change detection, commit, push, and cleanup run through `spawn_blocking` in Manager paths. This organization prevents normal branch overlap; it does not isolate hostile code.

The manager stores a per-project `GitProviderConfig` in project data, resolves its API token through `SecretsStore`, constructs the provider lazily, and caches it by project ID. `SecretUpdated` invalidates the cache. The factory currently constructs only Forgejo; other provider types return unsupported (`crates/tj-git/src/lib.rs:24-66`).

After Engineering completes, Manager resolves the exact task worktree, base SHA and changed files (including uncommitted/untracked work), validates plan/packet/task provenance plus real test-command execution and criterion mapping, then commits, pushes and verifies the remote SHA before opening one draft PR. Push/PR failures are durable failures, not success; retries reconcile the remote first and never duplicate a PR. TurboJET does not mark ready or merge. `crates/tj-api/tests/engineering_harness.rs` exercises this whole API→Manager→Flow→Stage→real-shell→publication path; the coordinator-only scenarios are in `crates/tj-manager/tests/publication_harness.rs`.

`SshKeyGuard` is Manager-owned. SSH-backed remotes require a project private key plus a project-scoped verified `known_hosts` file. Key/host files are created atomically with restrictive modes, and `GIT_SSH_COMMAND` enforces strict host checking, batch mode and project-only identity before clone/push, then removes temporary material on drop. A failed clone leaves an explicit recoverable provisioning state and never substitutes `git init`. Real-forge SSH evidence remains externally blocked.

## Secrets Boundary

`tj-secrets` defines async CRUD by `(project_id, key)`. `DbSecretsStore` in Manager implements it over synchronous SQLite calls using `spawn_blocking`. It provides AES-256-GCM storage at rest using a random 96-bit nonce and a 32-byte master key supplied as 64 hex characters in `TURBOJET_MASTER_KEY` (`crates/tj-manager/src/secrets_crypto.rs:6-51`).

Keys beginning with `__` are reserved for system use, including SSH material. This namespace is enforced at the user API boundary rather than inside the generic store. If the master key is missing or invalid, startup continues with `secrets_store: None`; secrets/SSH endpoints return unavailable and secret-driven Forgejo construction is skipped. This is degraded mode, not secure recovery of encrypted data (`crates/tj-manager/src/actor.rs:444-465`).

The separation of `SpawnResult`, `AppState`, and `ManagerHandle` removes a direct secret-retrieval API path from `SystemOperations`. Model-driven shell calls run under the local Landlock+seccomp boundary with an explicit environment allow-list, workspace/output-only writes and socket/session-escape denial; this is a narrow local boundary, not a container/mount namespace or a claim to contain deliberately hostile native code. HTTP secret/SSH routes require an instance administrator; diagnostic logging is credential-safe by regression test. `tj-ops` provides external-key backup/restore and versioned packaging under an enforced offline/quiesced lock; key rotation and complete security-audit controls remain unresolved.

## API, Persistence, and Logging Boundaries

The default server binds `0.0.0.0:8100`. Bearer sessions, project-membership middleware and instance-admin gates protect run/stream/secrets/account operations; bootstrap registration is limited to the first administrator. CORS remains permissive and the deployment is not approved for public/shared use (`crates/tj-api/src/access.rs`, `crates/tj-api/src/auth.rs`).

HTTP diagnostic logging retains request IDs/status/timing and redacts credential-bearing headers, cookies, query strings and bodies; SSE is forwarded incrementally. It is not a durable audit guarantee (`crates/tj-api/src/logging.rs`).

`tj-logs` can write and read structured JSONL data, and tracing includes run/flow/stage identifiers and timings. The HTTP log handlers are placeholders that currently return empty results rather than reading `tj-logs` (`crates/tj-api/src/log_handlers.rs:35-127`). There is no established metrics, SLO, alerting, or durable audit-event subsystem.

## Trust and Security Boundary

The current implementation assumes a trusted operator, trusted users, trusted repositories, and trusted model/tool instructions. Specifically:

- File tools constrain their own paths to a canonical workspace root.
- `ShellTool` bypasses that file-tool boundary by running arbitrary inherited-environment Bash.
- Worktrees organize git state but provide no security isolation.
- The service API is broad; project-scoped sessions and admin gates are implemented, but public/shared deployment is not approved.
- Model/provider, Zulip, Forgejo, git, filesystem, process environment, and SQLite are external trust/dependency surfaces.
- Secrets use AES-256-GCM storage at rest when the master key is available, but inherited Bash environment/host access, key rotation, audit and revocation remain incomplete security controls; backup/restore is provided by `tj-ops` with an external master key.

Therefore the current system must not be described as Docker/container-isolated, a general hostile-code sandbox, multi-tenant, or production-ready. The TJ-034 Landlock/seccomp boundary is narrow and local; it does not create mount namespaces and its real-forge/pilot behaviour is unproven.

## Failure and Recovery Semantics

- Stage timeout returns a timed-out/failed outcome after the runner wait expires. Shell timeout/cancellation terminates ordinary process-group descendants under the TJ-034 boundary; this is not a guarantee against deliberately hostile native code.
- User cancellation and watchdog timeout persist terminal state, abort/join the flow future and process supervisors, then clean up the known worktree. Confirmed shutdown failure preserves the workspace. Late events/answers are fenced (`crates/tj-manager/src/actor.rs`, `crates/tj-tools/src/processes.rs`).
- On startup, runs still marked `Running` are fenced as `interrupted`; no automatic `RunRequested`/model/tool/publication replay occurs. `run_recovery` records the durable resume flow/stage. An explicit retry starts a new attempt. Gate routing effects may be reconciled once; uncertain publication effects require acknowledgment (`crates/tj-db/src/recovery.rs`, `crates/tj-db/src/effects.rs`).
- Local DB updates, git commands, Forgejo calls, and notifications are not one transaction. Durable `pending_effects` records make gate routing resumable once; Engineering publication additionally has durable push/PR identities and reconciles `unknown` operations before retry. External effects still cannot be proven atomic with the local database.
- Verdict routing fails closed: a missing or unroutable verdict fails the flow loudly with a stage-naming error, and escalation requires human intervention. Missing ADC/prompt/schema/provider/model references fail admission. Engineering push/PR failures block draft-PR success and retain the workspace; they are never reported as completed publication.

## Known Limitations

- Discovery → Planning and one-task Engineering are locally implemented and scripted-tested, including deterministic plan validation, exact packet/plan approval, explicit task selection, test/review/acceptance evidence (test commands and criterion mappings verified against durable shell activity) and draft-PR publication. External CI/CD delivery is locally contracted but has no real deployment/rollback evidence; a real-forge SSH/PR pilot and external Planning pilot evidence are not established. Evidence belongs in [CAPABILITIES.md](CAPABILITIES.md).
- Shell execution fails closed without the narrow TJ-034 Landlock/seccomp boundary; this is not a mount namespace or a guarantee of safety for deliberately hostile native code.
- Restart re-enters a flow rather than resuming an exact checkpoint.
- SMG/ADC contracts deny unknown fields and fail closed; the supported schema vocabulary is limited.
- Non-interactive and interactive completion both validate structured output locally against the frozen schema and verify required artifacts.
- Git/Forgejo publication failures are fatal to Engineering readiness and retained for reconciliation; external effects remain nontransactional with the local database.
- Outer `WaitingForInput` flow semantics are dormant rather than removed; interactive waits live inside the stage.
- Direct stage-message and legacy merge-complete Manager routes are no-ops; interactive input uses Flow chat sessions.
- CORS restriction public/shared deployment approval, and operational monitoring/deployment controls are incomplete.
- Some legacy log reader routes are compatibility shells; durable support export/SLO work remains.
- Project provisioning now uses the explicit SSH-key/known-hosts guard and finite Git timeouts; real-forge SSH proof remains pending.
- ADK Git dependencies are revision-pinned in the root manifest but live provider/forge evidence remains absent.
