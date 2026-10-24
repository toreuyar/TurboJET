# TurboJET

TurboJET is a self-hosted, registry-driven workflow engine for AI-assisted software delivery. It runs explicitly registered workflow graphs with durable runs, stages, gates, artifacts, questions, and budgets, and exposes them through a browser UI, a conversational agent named **Chief**, and an HTTP API.

> **Project status: v1.0.0 source release, local/development maturity.** TurboJET is not production-ready. It has local and scripted integration evidence, but no production-hosted service guarantee, external security audit, real-forge publication proof, live deployment proof, or external user evaluation is claimed. See [Maturity and boundaries](#maturity-and-boundaries).

## What it does

- **Registry-driven workflows.** There is no built-in workflow, default SDLC graph, or filename-based profile inference. Operators configure `workflow_sources`; the shipped templates load the self-contained packages in `workflows/`, while a config with no sources starts with an empty registry.
- **Durable orchestration.** Runs are persisted in SQLite with immutable execution bundles, stage records, gate decisions, questions, artifacts, budgets, and recovery states.
- **Bounded execution.** Workflows declare stages, routes, retries, and limits. Routing is verdict-first and fails visibly instead of silently guessing.
- **Human authority at gates.** Gates bind an exact artifact, requirements packet, or plan revision. TurboJET records approvals and never marks a pull request ready or merges it.
- **Browser-first operation.** The UI covers setup/readiness, workflows, projects, runs, gates, packets, plans, delivery, and artifacts.
- **Core independence.** The default engine and test layer can run without shipped workflow, agent, schema, or example assets. Workflow packages and the local full SDLC are optional.

The shipped packages under `workflows/` are useful first examples. Each package owns its graph, agents, prompts, and schemas. The three generic templates are **experimental**; Discovery is the supported repository-free example, still with local/scripted evidence only. None is a built-in default.

## Architecture at a glance

```text
Browser UI / HTTP API / Chief
             |
             v
      Manager (durable state, lifecycle, gates, budgets)
             |
             v
      Flow (workflow routing)
             |
             v
      Stage (frozen ADC/prompt/schema/provider bundle)
             |
             v
      Runner (model loop) -> native tools (read/write/edit/bash)
             |
             v
      SQLite, artifacts, optional Git/forge, optional delivery adapter
```

Repository work and external delivery are capability options declared by a workflow; they are not required by the core engine. A Git worktree is a branch/work-file boundary, not a sandbox. Model-driven shell execution is constrained on supported Linux hosts by a local Landlock/seccomp boundary and otherwise refuses to run, but execution still occurs in the TurboJET process and is not container isolation.

See [Architecture](ARCHITECTURE.md) and the [capabilities matrix](CAPABILITIES.md).

## Quickstart

This path is for local development on Linux. It is not a production install.

### Prerequisites

- Linux, preferably Ubuntu 24.04 x86-64
- Rust **1.98.0** (declared in the workspace)
- Node **22.23.2** and npm 10 (`.nvmrc`, `webui/package.json`)
- Git, a C/C++ toolchain, `pkg-config`, and `perl`
- A provider API key compatible with the configured provider

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config perl git
```

### 1. Choose a workflow source

Start from the loopback-bound self-hosted template:

```sh
cp config/turbojet.selfhosted.json config/turbojet.local.json
```

The template already loads the four shipped packages in `workflows/`:

```json
"workflow_sources": [
  { "id": "examples", "path": "workflows", "kind": "directory" }
]
```

`config/turbojet.local.json` is Git-ignored and has the same config base as the template, so `workflows` resolves from the repository root. A directory source is scanned non-recursively for `*.smg.json` and immediate package subdirectories; every shipped package has a root `workflow.smg.json`.

To start empty instead, set `"workflow_sources": []`. TurboJET still starts and serves the UI/API, but project creation returns a clear `409` until a workflow is installed. To add your own workflow, create a self-contained package under `workflows/<name>/` with its graph, agents, prompts, and schemas, then point a `kind: "package"` source at it or place it under the configured directory. See [Workflow authoring](docs/workflow-authoring.md).

Do not point a shared source at `config/local` unless you explicitly intend to load local-only graphs.

### 2. Build the backend and UI

```sh
cargo build --locked -p tj-api
(cd webui && npm ci && npm run build)
```

### 3. Configure required environment

Use your shell or create a Git-ignored `.env` at the repository root:

```sh
cat > .env <<'EOF'
ZAI_API_KEY=replace-with-your-provider-key
TURBOJET_MASTER_KEY=replace-with-64-lowercase-hex-characters
EOF
chmod 600 .env
```

`TURBOJET_MASTER_KEY` encrypts project secrets at rest. Generate it once with `openssl rand -hex 32` and store it securely; changing or losing it makes existing encrypted secrets unreadable. Never commit `.env`, provider keys, or the master key.

The configured provider is `zai` with model `glm-5.1` and API-key variable `ZAI_API_KEY`. You can change the provider/model in your local config if you have a verified compatible endpoint. Readiness reports whether the named key is present without printing it.

### 4. Start TurboJET

```sh
export TURBOJET_HOME="$PWD/.turbojet"
export TURBOJET_WEBUI_PATH="$PWD/webui/dist/index.html"

cargo run --locked -p tj-api -- config/turbojet.local.json
```

Open <http://127.0.0.1:8100>. Stop with `Ctrl+C`; SIGTERM triggers a graceful shutdown.

### 5. Create the first administrator

The first successful registration becomes the instance administrator. Either use the UI or call the API directly:

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"choose-a-long-password"}'
```

The response contains a bearer token. Save it in your password manager or shell session; do not commit it. Once one user exists, open registration is closed.

### 6. Create a project and run

In the UI, open **Workflows**, choose a catalog entry, and create a project. Or use the API:

```sh
export TOKEN='paste-the-token-from-registration'

# List registered catalog entries and immutable workflow versions.
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8100/v1/profiles

# `agent-loop` is registered by the shipped `workflows/` source above.
curl -sS -X POST http://127.0.0.1:8100/v1/projects \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"First project","catalog_id":"agent-loop"}'
```

Use the returned `project.id` and `lane.id` to start a run:

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/pipeline/runs \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"description":"Summarize the supplied notes","project_id":"proj_...","lane_id":"lane_..."}'
```

Interactive stages ask through the durable question channel. Gates show the exact bound artifact and hash before approval or rejection.

### UI development mode

Run the API on port `8100` as above, then:

```sh
cd webui
npm ci
npm run dev
```

Open <http://127.0.0.1:5173>. Vite proxies `/v1` and `/api` to `http://127.0.0.1:8100`.

Readiness is available at `GET /api/health` (process), `GET /api/ready` (public dependency summary), and authenticated `GET /v1/readiness`. Process liveness alone does not mean the instance is ready; an empty workflow registry is reported as degraded.

For full setup, configuration, workflow authoring, operations, and evidence boundaries, see the [documentation index](docs/index.md), [setup guide](docs/setup.md), [configuration reference](docs/configuration.md), [workflow authoring guide](docs/workflow-authoring.md), and [self-hosted runbook](docs/runbook-self-hosted.md).

## Core and optional layers

| Layer | Required? | Current status |
|---|---|---|
| Core engine, API, durability, security boundaries | Yes | Implemented and covered by the default/asset-free test suites in the current working tree. No release certification. |
| Browser UI and Chief | Optional build, supported local surface | Implemented locally; browser journeys use mocked and scripted backends. |
| Shipped workflow packages (`workflows/*/`) | No | Discovery plus three repository-free generic templates, each self-contained (graph, agents, prompts, schemas). Discovery is the historical supported example; the generic templates are **experimental**. Scripted-test evidence only; none is a built-in default. |
| Local full-SDLC package (`config/local/`) | No | Operator-private, ignored/untracked, **experimental**, never packaged or selected by filename. Built explicitly with `scripts/import-local-sdlc.sh`; not present in a fresh clone. |
| Zulip chat adapter | No | Present but **disabled** at startup until an authenticated account-mapping adapter exists. Browser/Chief is the supported surface. |
| Forgejo/SSH repository publication | No | Local disposable-remote and mock-protocol evidence exists. **Real SSH/forge/PR proof is BLOCKED.** |
| External CI/CD delivery adapter | No | Forgejo-Actions-compatible contracts and signed callback drills exist locally. **Real deployment/rollback evidence is BLOCKED.** |
| Docker/container execution | No | Not the current runtime. Historical Docker files are not a supported isolation claim. |

The core test layer can be checked independently of shipped assets:

```sh
bash scripts/check-asset-free.sh
```

The optional example layer is explicit:

```sh
bash scripts/check-examples.sh
```

The latter may report the private local SDLC operational regression as `BLOCKED` when the derived `config/local/full-sdlc.package/` is absent. That is expected in a fresh public checkout; it is not a hidden fallback or a core failure. Run `bash scripts/import-local-sdlc.sh derive` first to build the package from a private original graph.

## Maturity and boundaries

TurboJET is a local, trusted-host system. In particular:

- No production, multi-tenant, shared-host, or public-internet deployment is approved. The default tracked config binds `0.0.0.0:8100`; use the loopback self-hosted template unless you have deliberately designed network access controls.
- CORS is permissive, and bearer-token sessions are not a complete multi-tenant security model.
- Worktrees are not sandboxes. Model-driven shell commands run in the host process under a local Linux boundary; the boundary is not container or hostile-code isolation.
- The exact model conversation is not resumed after restart. Interrupted work requires an explicit retry and starts a new attempt.
- The supported output-schema vocabulary is intentionally small, not full JSON Schema.
- Real provider behavior, cost, forge/SSH operation, delivery, and external user outcomes remain unproven; missing usage is reported as `unknown`, never as zero.
- Local scripted and browser evidence is not external validation.

See [CAPABILITIES.md](CAPABILITIES.md) and [security policy](SECURITY.md) before exposing an instance beyond localhost.

## Development and checks

The default workspace suite is asset-free and does not need shipped workflows, agents, schemas, or examples:

```sh
bash scripts/checks.sh rust
```

Web unit/build and mocked-browser checks:

```sh
bash scripts/checks.sh web
```

Optional example and private local-SDLC layers:

```sh
bash scripts/check-examples.sh
```

Dependency policy: Cargo and npm installs are locked (`Cargo.lock`, `webui/package-lock.json`); use `--locked` and `npm ci`. See [docs/checks.md](docs/checks.md) for the supported baseline and exact commands.

## Documentation

- [Introduction and concepts](docs/introduction.md)
- [Documentation index](docs/index.md)
- [Setup guide](docs/setup.md)
- [Configuration reference](docs/configuration.md)
- [Workflow authoring](docs/workflow-authoring.md)
- [Generic example workflows](docs/generic-workflows.md)
- [Architecture](ARCHITECTURE.md)
- [Checks and verification](docs/checks.md)
- [Capabilities and evidence](CAPABILITIES.md)
- [Self-hosted runbook](docs/runbook-self-hosted.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## License

TurboJET is released under the [MIT License](LICENSE).

Copyright (c) 2026 TÖRE ÇAĞRI UYAR.

Third-party dependencies retain their own licenses; see `Cargo.lock` and `webui/package-lock.json` for the locked dependency set.
