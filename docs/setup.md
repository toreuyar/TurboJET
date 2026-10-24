# Setup guide

This guide builds and runs TurboJET locally from a source checkout. It is a local development setup, not a production installation. The supported check baseline is Ubuntu 24.04 x86-64 with Rust 1.98.0, Node 22.23.2, npm 10, Git, a C/C++ toolchain, `pkg-config`, and Perl.

For exact dependency and browser requirements, see [Reproducible checks](checks.md). For packaging, backup, and restore, see the [self-hosted runbook](runbook-self-hosted.md).

## 1. Build from source

```sh
git clone <repository-url> turbojet
cd turbojet

cargo build --locked -p tj-api
```

The default engine does not require any shipped workflow, agent, schema, or example asset. The UI is a separate build:

```sh
cd webui
npm ci
npm run build
cd ..
```

`npm ci` requires the committed `webui/package-lock.json`; do not substitute an unlocked install when reproducing the supported check baseline.

## 2. Choose the instance configuration

The tracked templates are:

| File | Intent |
|---|---|
| `config/turbojet.selfhosted.json` | Loopback bind (`127.0.0.1:8100`), state under `$TURBOJET_HOME`, loads the shipped `workflows/` packages. Recommended starting point. |
| `config/turbojet.json` | Development template with a broader bind (`0.0.0.0:8100`) that also loads `workflows/`. Do not expose it without deliberate network controls. |

Create an ignored local copy so your operator edits never appear as tracked changes:

```sh
cp config/turbojet.selfhosted.json config/turbojet.local.json
```

`config/turbojet.local.json` is covered by `/config/*.local.json` in `.gitignore` and has the same config base as the template.

The config base is the **parent of the directory containing the config file**. For both files above, that is the repository root. If you move a config into a deeper directory such as `config/local/`, the base becomes `config/` and a relative workflow path such as `workflows` resolves to `config/workflows`, which does not exist. Keep the config directly under `config/`, or use absolute paths deliberately.

## 3. Register at least one workflow

There is no default graph. The shipped templates already declare:

```json
"workflow_sources": [
  { "id": "examples", "path": "workflows", "kind": "directory" }
]
```

The `workflows` directory is scanned non-recursively, sorted by path. It contains the Discovery package and the three experimental generic packages. Each is self-contained: its graph references only its own agents, prompts, and schemas, which do **not** need separate source entries.

An empty `workflow_sources` list is also valid: the service starts with an empty registry and project creation returns a `409` until a workflow is installed.

To register your own package explicitly:

```json
"workflow_sources": [
  { "id": "my-flow", "path": "workflows/my-flow", "kind": "package" }
]
```

To register one legacy graph file instead:

```json
"workflow_sources": [
  { "id": "my-flow", "path": "workflows/my-flow.smg.json", "kind": "file" }
]
```

Rules that matter for onboarding:

- Source `id` values must be unique.
- A missing or invalid configured source fails startup; it is not silently skipped.
- `smg_path` is a deprecated single-file alias. Use `workflow_sources`.
- A `directory` source imports immediate package subdirectories with a root `workflow.smg.json`; deeper packages must be named separately.
- A `package` source resolves agents/prompts/schemas only inside that package. The legacy `adc_dir` field is not used.
- Only one imported catalog may set `"default": true`; order and filenames never imply a default.
- Each graph must declare `catalog.id`. See [Workflow authoring](workflow-authoring.md).
- Do not point a shared/public source at `config/local` unless you intend to register private local packages.

See [Configuration](configuration.md) for every config field and environment override.

## 4. Configure secrets and environment

TurboJET reads process environment variables, then a first-found `.env` at the repository root or `$TURBOJET_HOME/.env`. Process environment values win over `.env` values.

Minimum local setup:

```sh
cat > .env <<'EOF'
ZAI_API_KEY=replace-with-your-provider-key
TURBOJET_MASTER_KEY=replace-with-64-lowercase-hex-characters
EOF
chmod 600 .env
```

- The configured example provider is `zai`, base URL `https://api.z.ai/api/coding/paas/v4`, default model `glm-5.1`, and API-key variable `ZAI_API_KEY`. This path is configured but not externally verified.
- `TURBOJET_MASTER_KEY` must be 64 hexadecimal characters and is used for AES-256-GCM project-secret storage. Generate it once with `openssl rand -hex 32`, store it outside the repository, and reuse it for backup/restore. Losing it makes existing encrypted secrets unreadable.
- `.env` is Git-ignored. Never commit it or paste keys into issues, logs, or support exports.

Optional:

```sh
export TURBOJET_HOME="$PWD/.turbojet"                 # default: ~/.turbojet
export TURBOJET_WEBUI_PATH="$PWD/webui/dist/index.html"
export RUST_LOG=turbojet=info                         # tracing filter override
```

The database defaults to `$TURBOJET_HOME/state.db`. `start.sh` is a development convenience; it builds the binary and sources `config/.env` if present. Prefer explicit environment management rather than relying on a checked-out `.env`.

## 5. Start the service

```sh
export TURBOJET_HOME="$PWD/.turbojet"
export TURBOJET_WEBUI_PATH="$PWD/webui/dist/index.html"

cargo run --locked -p tj-api -- config/turbojet.local.json
```

Open <http://127.0.0.1:8100>. With the config above, the UI is served at `/`; without `TURBOJET_WEBUI_PATH`, `/` returns the JSON API descriptor.

Development UI alternative: keep the API on `8100`, then run `cd webui && npm ci && npm run dev` and open <http://127.0.0.1:5173>. Vite proxies `/v1` and `/api` to port `8100`.

Shutdown with `Ctrl+C` or SIGTERM. The service attempts to join active execution and preserve work that was not fully published.

## 6. Bootstrap the first administrator

The first registration succeeds only while the user table is empty and creates an instance administrator. Registration then closes.

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"choose-a-long-password"}'
```

The response includes `token` and the user object. Keep the token private. Usernames must be at least three characters; passwords must be at least eight. For subsequent users, sign in as an administrator and use the user-management surface or API.

## 7. Verify readiness

```sh
curl -sS http://127.0.0.1:8100/api/health
curl -sS http://127.0.0.1:8100/api/ready
```

- `/api/health` only proves that the process is serving.
- `/api/ready` is the public dependency summary; `/v1/readiness` is the authenticated checklist used by the UI.
- `blocked` indicates a database/workspace/provider-key failure.
- `degraded` indicates a missing master key, built UI, or workflow registry. An empty registry is a supported start, not a ready product.
- Values are not printed; only presence and named variables are reported.

Example authenticated check:

```sh
export TOKEN='paste-token'
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8100/v1/readiness
```

## 8. Create a project and first run

Use the UI: **Workflows** lists registered catalog entries; **Use this template** or **+ Project** creates a project pinned to an exact workflow version. Then start a run from the Default lane.

Or use the API:

```sh
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8100/v1/profiles
```

Pick a `catalog_id` from the response. With the shipped `workflows/` source loaded, `agent-loop`, `clarify-produce-review`, `research-synthesize-verify`, and `discovery` are available. They are example registrations, not built-ins.

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/projects \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"First project","catalog_id":"agent-loop"}'
```

Use the returned `project.id` and `lane.id`:

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/pipeline/runs \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"description":"Summarize the supplied notes","project_id":"proj_...","lane_id":"lane_..."}'
```

For `research-synthesize-verify`, copy supplied files into the project workspace's `inputs/` directory before starting. The workflow's research stage reads only that directory and verifies citations against the supplied files; it does not use the web.

## 9. Optional local full-SDLC package

The full SDLC package is not included in a fresh clone or a versioned package. If an operator has a private `config/local/full-sdlc.smg.json`, they can derive a self-contained package and configure it explicitly:

```sh
bash scripts/import-local-sdlc.sh derive
bash scripts/import-local-sdlc.sh configure --config config/turbojet.local.json
# Restart the service so the new source is loaded, register/import it, then pin lanes:
TJ_API_BASE=http://127.0.0.1:8100 TJ_API_TOKEN=... \
  bash scripts/import-local-sdlc.sh import-pin --lane <lane-id>
```

`derive` verifies the original graph hash, writes `config/local/full-sdlc.package/` with the graph, agents, prompts, and schemas, and leaves the original unchanged. The package remains experimental and is excluded from versioned packages. Real repository/forge operation remains blocked. Do not treat local scripted publication evidence as production proof.

## 10. Run checks

```sh
# Default core suite; no shipped workflow/agent/schema/example assets required.
bash scripts/checks.sh rust

# Locked frontend install, unit tests, production build, mocked browser checks.
bash scripts/checks.sh web

# Stronger independence check in a disposable asset-free copy.
bash scripts/check-asset-free.sh

# Optional shipped examples and, when present, the private local SDLC regression.
bash scripts/check-examples.sh
```

Optional checks may report a missing private local SDLC package as `BLOCKED`; that is expected in a fresh checkout. Never substitute optional scripted evidence for live or external proof.

## State and cleanup

Runtime state is under `$TURBOJET_HOME`; the configured workspace base is normally under the same directory. For a clean disposable instance, point `TURBOJET_HOME` at a new directory rather than deleting an existing state tree. If you intentionally remove state, stop the service first and retain anything you may need for recovery. Do not run destructive cleanup against a live database or an unpublished worktree.
