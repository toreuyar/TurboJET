# Self-hosted TurboJET runbook (TJ-025)

**Scope:** one supported Linux host, local SQLite + artifact directory. This is
a local/development operations runbook. It does not authorize a production
install, deployment or release. All drills below must use disposable
directories.

## 1. Build a versioned package

```bash
CARGO_HOME=/tmp/opencode/turbojet-clean-cargo \
CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target \
  cargo build --locked --offline -p tj-api -p tj-ops

(cd webui && npm ci && npm run build)

target/debug/tj-ops package \
  --out /tmp/turbojet-package \
  --binary /tmp/opencode/turbojet-clean-target/debug/tj-api \
  --webui webui/dist/index.html \
  --config config \
  --workflows workflows \
  --version 1.0.0-local
```

The result is `turbojet-<version>/` with `bin/tj-api`, `share/webui/index.html`,
config copies, the self-contained `workflows/` packages, `VERSION`,
`manifest.json`, `SHA256SUMS` and a local `start.sh`.
It is not installed anywhere and is not a release artifact. `scripts/package-release.sh`
wraps the same steps and refuses to publish. The historical `docker/` development
files and `scripts/test_server.sh` are not release packaging; do not present them
as the supported host install.

Packaging is an explicit policy, not a copy of whatever Git ignores:

- `config/local/**` is always excluded, even when it is untracked/ignored. The
  private local SDLC original graph and derived package never enter a package.
- `.env*`, symlinks, `.git`, `node_modules`, `.config`, runtime state
  (`.turbojet`, `.factory`, `.isolator-staging`, `.opencode`), `target`,
  `check-results`, web test reports, `*.db*` and `*.log` are excluded.
- The four self-contained packages under `workflows/` are included by policy.
  `manifest.json.exclusions` records the policy; old manifests without that
  field still read as format v1.
- The packaged `config/turbojet.selfhosted.json` declares
  `workflow_sources: [{ "path": "workflows", "kind": "directory" }]`, relative
  to the package root. It registers the shipped packages; no built-in or hidden
  default is selected. A package built without `workflows/`, or a config with
  no sources, still starts with an empty registry and a useful Chief/UI.

Prove the distribution boundary from disposable directories:

```bash
bash scripts/package-release.sh
bash scripts/package-smoke.sh /tmp/opencode/turbojet-package/turbojet-1.0.0 all
```

The `empty` mode strips all shipped workflow/agent/schema/example assets from
the copy, then checks health, UI, degraded `workflows` readiness, empty catalog,
and the actionable 409 when no project can be created. The `shipped` mode keeps
the package's `workflows/` packages, verifies all four catalogs import with
`source_kind: "package"` and non-zero `asset_count`, and pins a project lane to
the exact `discovery` workflow version. The `synthetic` mode injects one exact
catalog package, proves import/catalog listing and project/lane pinning, and
never calls a live provider. `both` remains the historical empty+synthetic pair.
The script uses a disposable package copy, a dummy provider key and SIGTERM
shutdown.

## 2. Required environment

| Variable | Purpose |
|---|---|
| `TURBOJET_HOME` | State home; database default `$TURBOJET_HOME/state.db` |
| `TURBOJET_WEBUI_PATH` | Built UI `index.html` |
| provider API-key env | Named by `providers[].api_key_env`; readiness names the exact variable |
| `TURBOJET_MASTER_KEY` | 64 hex characters; external key for encrypted project secrets |

Never put the master key or `.env` in a backup, repository or support export.
Readiness reports whether the provider key/master key are present without
printing their values.

Optional cost controls: set `budgets.cost_micros` and provider
`input_cost_micros_per_million` / `output_cost_micros_per_million`. Missing
provider usage remains `unknown` and is never charged as zero. `budgets.model_calls`
is the call cap; both caps stop new model work when exhausted. Retry budgets are
per provider class (`rate_limit_retries`, `transient_retries`, `retry_base_ms`);
credentials never retry and no tool is replayed for a transport retry.

## 3. Readiness and startup

```bash
TURBOJET_HOME=/srv/turbojet \
TURBOJET_WEBUI_PATH=/srv/turbojet/share/webui/index.html \
TURBOJET_MASTER_KEY=... PROVIDER_API_KEY=... \
  /srv/turbojet/bin/tj-api /srv/turbojet/config/turbojet.json
```

- `GET /api/health` — process alive.
- `GET /api/ready` — public dependency summary (no secrets).
- `GET /v1/readiness` — authenticated copy for the UI checklist.
- `status: blocked` means database/workspace/provider-key failure; `degraded`
  means secrets master key, built UI or workflow registry is missing. Do not
  declare readiness from process liveness alone. An empty registry is a
  supported start: `checks[].name == "workflows"` is `ok: false` with
  “No workflow installed; configure a workflow source or import a workflow.”
  Configure a source (or call `POST /v1/workflows/import` after adding a package
  to an already configured source) before creating a project. The shipped
  templates load `workflows/` and normally report ready; there is no hidden
  fallback graph if those sources are absent.
- The canonical conversational agent id is `chief` (config and
  `/api/ready.provider.agent_id`). Legacy `id: "chef"` and
  `TURBOJET_AGENT_CHEF_MODEL` are accepted only during the documented
  compatibility window with a warning; canonical `TURBOJET_AGENT_CHIEF_MODEL`
  takes precedence. No database migration is involved.

Startup runs schema migrations before accepting requests. Every migration is
idempotent; `PRAGMA user_version` advances only after all migrations succeed.
An interrupted/failed migration leaves the previous version and startup fails
closed.

## 4. Backup (supported policy: offline/quiesced only)

**Supported procedure: stop the service → take the backup → start the service.**

```bash
# 1. Stop writers. SIGTERM joins active execution and closes admission.
sudo systemctl stop turbojet          # or the equivalent for your supervisor

# 2. Offline backup. This takes the same <state.db>.lock a running server holds
#    and keeps it for the entire DB + artifact snapshot.
TURBOJET_MASTER_KEY=... \
target/debug/tj-ops backup \
  --db /srv/turbojet/state.db \
  --artifacts /srv/turbojet/workspaces \
  --out /srv/backups/turbojet-$(date -u +%Y%m%dT%H%M%SZ)

# 3. Verify while still offline, then start again.
TURBOJET_MASTER_KEY=... target/debug/tj-ops verify --backup /srv/backups/turbojet-...
sudo systemctl start turbojet
```

- `tj-ops backup` acquires the exclusive instance lock for the whole snapshot.
  If a TurboJET process is still running it fails explicitly with
  `backup refused: database ... is in use ... (stop -> backup -> start)`; it
  never produces a hot backup with mixed DB/artifact points in time.
- The database snapshot uses SQLite `VACUUM INTO` (consistent under WAL), not a
  raw file copy. Artifacts are hashed and copied while the lock prevents a
  service from writing them. Symlinks are refused so the backup cannot escape
  the artifact root.
- `manifest.json` records `offline_exclusive_lock: true`, format version,
  TurboJET version, schema version, database/artifact hashes and explicit
  exclusions (`.env`, master key, runtime logs).
- Secrets remain encrypted ciphertext. The manifest records `secret_rows` and a
  master-key fingerprint, never the key. `TURBOJET_MASTER_KEY` is required to
  verify decryption on restore.
- A filesystem snapshot is an optional alternative for a stronger host-level
  guarantee, but it is not required by this supported path. Do not run a
  hot backup against a live service and call it consistent.

Verify without changing state:

```bash
TURBOJET_MASTER_KEY=... target/debug/tj-ops verify --backup /srv/backups/turbojet-...
```

## 5. Restore and failed-upgrade recovery

Move the current state aside first; restore refuses a non-empty target unless
`--force` is explicit.

```bash
target/debug/tj-ops restore \
  --backup /srv/backups/turbojet-... \
  --home /srv/turbojet-restored \
  --no-key-check          # ciphertext restore; verify the key separately
TURBOJET_MASTER_KEY=... target/debug/tj-ops verify --backup /srv/backups/turbojet-...
```

Restore is refused while a `tj-api` process holds the database lock
(`<state.db>.lock`). Stop the server first; `tj-ops` stages the database and
artifact tree and installs them with atomic renames, so a failed copy does not
leave a partially written target. Restored `state.db` and artifact files are
created owner-only (`0600`, directories `0700`). Use `--no-key-check` only when
the key is intentionally not present on the restore host; run `verify` with the
key before serving. The restored directory gets `restore-manifest.json`.

Failed upgrade drill (tested in `crates/tj-ops`):

1. Create a disposable home with a project, artifact and encrypted secret.
2. Back it up and verify.
3. Corrupt or replace the live `state.db` to simulate a failed upgrade.
4. Restore into a fresh directory, open the database and confirm the project,
  artifact, secret ciphertext and schema are present.
5. Only then replace the live directory during an approved maintenance window.

## 6. Graceful shutdown (SIGTERM)

Send SIGTERM to the API process, not SIGKILL. The signal path closes new run
admission, stops active executions through the joined cancellation path, records
unconfirmed shutdown if cleanup cannot complete, and preserves affected
workspaces for recovery. Confirm the process exited and inspect runs marked
`interrupted`; restart never auto-replays model/tool/publication work.

SIGKILL is only a last resort and is treated like a crash: startup fences
running work as interrupted and requires an explicit retry.

## 7. Retention and diagnostics

- `activity_retention_days` (default 30) prunes durable stage activity and
  provider attempts at startup. Runs, artifacts, decisions, packets and secrets
  are never pruned by this setting.
- `POST /v1/admin/activity/prune` lets an administrator run the same bounded
  cleanup; `{"before":"<RFC3339>"}` overrides the cutoff.
- Activity is redacted at write and export. Project members can read their run
  activity/diagnostics; support export requires project access.
- The public `/api/ready` and authenticated `/v1/readiness` are the source of
  truth for missing dependencies; legacy `/api/logs/*` routes now read durable
  redacted activity instead of returning empty placeholders.

## 8. Upgrade checklist

1. Read release notes and confirm the versioned package checksums.
2. Take and verify a backup.
3. Stop with SIGTERM and confirm joined shutdown.
4. Apply the new package to a staging path, not over the live directory.
5. Start against a copy of the database and check `/api/ready`, migrations and a
   read-only run/budget page.
6. Only after an approved window, switch the live path; keep the previous
   package and backup for rollback.
7. If readiness, migration or a smoke journey fails, stop and restore the
   verified backup. Never hand-edit production SQLite rows.

## 9. External delivery, verification and rollback

TJ-039/TJ-040 delegate execution to a Forgejo-Actions-compatible external
workflow. TurboJET binds the exact publication/task/plan/packet/PR/commit,
dispatches explicitly, and records signed CI/deployment/verification/observation
callbacks. It never deploys, merges, marks a PR ready or releases by itself.

- Configure `delivery` as documented in
  [delivery-integration.md](delivery-integration.md). Missing token/callback
  secret fails startup closed; an absent block leaves history inspectable but
  disables dispatch/polling/callbacks.
- Staging is an explicit operator request. Production additionally requires an
  immutable decision bound to the exact revision and a `ready` staging release.
  A failed CI/test, verification or observation blocks the recommendation.
- `unknown` means the external state is unproven. Reconcile with a signed
  callback or explicit poll; never edit the database to mark success. Restart
  reconciliation marks in-flight work unknown. Re-dispatch is explicit and
  acknowledged, with the same operation key for workflow-side deduplication.
- Rollback requires an authorized exact target revision and independent
  verification against the actual deployed target plus health evidence.
- Operate the real drill in
  [delivery-live.md](../tests/qa/scenarios/delivery-live.md). The local
  disposable adapter proves only local contract logic. TJ-039/TJ-040 remain
  externally BLOCKED until real run URLs, deployed-revision and health evidence
  exist.
