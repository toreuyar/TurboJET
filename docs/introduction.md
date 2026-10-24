# Introduction to TurboJET

TurboJET is a self-hosted workflow engine for supervised, AI-assisted software delivery. Instead of embedding one fixed pipeline, it loads explicit workflow graphs and executes them with durable state, bounded autonomy, and human decisions at consequential gates.

**Status:** v1.0.0 source release, local/development maturity. The current worktree has substantial local and scripted evidence, but no production deployment, real-forge pilot, external security audit, live external CI/CD proof, or independent user evaluation is claimed. See the [capabilities matrix](../CAPABILITIES.md) and [checks](checks.md).

## The problem it addresses

Long-running AI work becomes difficult to trust when it is a single opaque conversation. TurboJET makes the work inspectable and resumable:

- a workflow declares the stages, routes, tools, limits, and gates;
- each run freezes the exact graph, prompts, schemas, provider/model names, and limits it admitted;
- artifacts, questions, gate decisions, and budgets are persisted;
- failures, exhausted limits, and missing evidence become visible states rather than silent success;
- humans approve exact artifacts, plans, and packets and retain merge/release authority.

## Core concepts

| Term | Meaning |
|---|---|
| **Workflow package** | A self-contained directory with one `workflow.smg.json` plus its agents, prompts, and schemas. Shipped packages live under `workflows/`; registry imports freeze their bytes. |
| **Workflow** | A JSON state-machine graph (SMG) loaded from an explicitly configured `workflow_sources` entry (`file`, `directory`, or `package`). |
| **Catalog entry** | The workflow's declared identity (`catalog.id`), label, maturity, and optional default. The registry never infers a workflow from a filename. |
| **Project** | A workspace created from one registered workflow. It may be repository-free or repository-backed only when the workflow declares that capability. |
| **Lane** | A project workflow instance pinned to an exact immutable workflow version. |
| **Job / Run** | One durable execution of a lane against a project. |
| **Flow** | A phase inside the workflow, containing stages and/or a human gate. |
| **Stage** | One AI-executed step with a frozen agent definition, prompt, schema, tools, provider/model, timeout, and retry bound. |
| **Gate** | A human decision bound to an exact artifact, requirements packet, or plan revision and content hash. |
| **Chief** | The optional conversational agent that can inspect workflows, projects, runs, gates, and delivery state through tools. It does not own routing or silently approve work. |

## What exists today

The local runtime includes:

- a Rust/Axum/Tokio service with synchronous SQLite persistence behind short, transactional lifecycle operations;
- an asynchronous Manager → Flow → Stage → Runner execution chain;
- immutable execution bundles, durable questions, cancellation, budgets, restart/interruption handling, and gate effects;
- bearer sessions, project membership, instance administrators, first-admin bootstrap, and encrypted project secrets at rest;
- a React/Vite browser UI and HTTP/SSE API;
- native tools with path validation; model-driven shell execution fails closed unless a local Landlock/seccomp boundary is available;
- optional repository provisioning, draft-PR publication, and external delivery contracts.

Workflows are never inferred. The shipped templates load the four self-contained packages under `workflows/`; a configuration with no `workflow_sources` starts with **no workflows**. That empty state is intentional and supported: the service, UI, readiness checks, and first-admin flow still work, but project creation waits for a workflow.

## What is optional

- **Discovery and generic examples** under `workflows/` are shipped packages, not the engine. They are not required for the core suite.
- **Full SDLC** is an operator-local, ignored original graph under `config/local/` plus a derived self-contained package. It is experimental, absent from a fresh clone, never packaged, and not selected by filename.
- **Zulip** is present but disabled at startup until authenticated account mapping exists. The browser and Chief are the supported local surfaces.
- **Forgejo/SSH publication** has local disposable-remote and mock-protocol evidence only. Real SSH/forge/PR operation is blocked.
- **External delivery** delegates execution to a Forgejo-Actions-compatible system with signed callbacks. Only local deterministic drills exist; real deployment and rollback are unproven.

None of these optional layers should be read as a production-readiness claim.

## Architectural shape

```text
Browser / API / Chief
          |
          v
   Manager: durable runs, gates, budgets, recovery, projects
          |
          v
   Flow: graph routing, bounded steps and iterations
          |
          v
   Stage: frozen ADC/prompt/schema/provider, artifacts, questions
          |
          v
   Runner: in-process model loop and native tools
          |
          +--> SQLite state
          +--> run workspaces and artifacts
          +--> optional Git/forge and delivery adapters
```

The [architecture document](../ARCHITECTURE.md) preserves the longer design history; the [capabilities matrix](../CAPABILITIES.md) records what the working tree does and does not prove.

## Security posture in one paragraph

TurboJET is a trusted-host system, not a multi-tenant sandbox. The HTTP listener, CORS policy, bearer sessions, and local execution boundary are suitable for deliberate local/self-hosted operation only. A worktree is not a security boundary. The local shell boundary reduces accidental host access but is not promised to contain deliberately hostile native code. Do not expose an instance to the public internet or untrusted users without a separate deployment and security design. Read [SECURITY.md](../SECURITY.md).

## Where to go next

- [Setup](setup.md) — build and run locally.
- [Configuration](configuration.md) — config file, environment, workflow sources, and optional adapters.
- [Workflow authoring](workflow-authoring.md) — add a workflow, agents, prompts, schemas, and gates.
- [Generic workflows](generic-workflows.md) — shipped experimental templates and limits.
- [Self-hosted runbook](runbook-self-hosted.md) — package, backup, restore, and operations drills.
- [Documentation index](index.md) — full map, including historical records.
