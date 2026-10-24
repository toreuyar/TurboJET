# Reproducible checks

## Supported baseline

- Ubuntu 24.04, x86-64.
- Rust **1.98.0** (CI pins this version; workspace manifests declare Rust 1.98).
- Node **22.23.2** (`.nvmrc`) and npm 10.
- Git, a C/C++ build toolchain, `pkg-config`, and Perl (vendored OpenSSL build).
- Chromium and OS libraries installed by the **locked** Playwright package.

Install Rust/Node using their normal installers. On Ubuntu the build packages are:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config perl git
```

Then run:

```sh
TJ_INSTALL_BROWSER=1 bash scripts/checks.sh
```

This runs locked Rust tests, a clean `npm ci`, frontend unit tests, the production
TypeScript/Vite build, browser prerequisite validation, and **eight mocked browser
tests**. Browser installation may request system-package privileges. Once browser
dependencies are installed, use `bash scripts/checks.sh` without the flag.

Use `bash scripts/checks.sh rust` or `bash scripts/checks.sh web` for a relevant
subset. Set `TJ_RUN_BACKEND_E2E=1` on `web`/`all` to also run the real-backend
scripted browser journeys; they remain local-fixture evidence, not live proof. `TJ_OFFLINE=1` enables Cargo's offline mode only when dependencies were
previously fetched. Normal checks use the network as needed and do not depend on
any particular user's Cargo cache. `CARGO_HOME`, `CARGO_TARGET_DIR`, and
`RUSTUP_TOOLCHAIN` retain their standard meanings. To select the exact CI compiler
locally, install it with rustup and set `RUSTUP_TOOLCHAIN=1.98.0` for the check command.

The script creates isolated temporary application-state/workspace/temp directories
and removes them on exit. Logs remain under `check-results/` (override with
`TJ_CHECK_RESULTS`). Browser HTML reports, failure traces, and screenshots are
under `webui/playwright-report/` and `webui/test-results/`. Generated output is ignored
by Git. GitHub Actions runs the same script and uploads results even after failure.

## Batch 4 packaged-workflow final matrix (2026-10-25)

This is the current working-tree matrix for the self-contained workflow-package
design. All commands used `CARGO_HOME=$HOME/.cargo`; Rust commands added
`--locked --offline`. The packaged distribution was rebuilt after the workflow
packages became package assets. Counts are actual:

| Check | Result | Log |
|---|---|---|
| `cargo test --workspace --locked --offline` | **508 passed**, 0 failed, 63 result targets | `/tmp/opencode/pkg-batch4-rust.log` |
| `cargo check --workspace --all-features --locked --offline` | pass | `/tmp/opencode/pkg-batch4-all-features.log` |
| `bash scripts/check-asset-free.sh` | Rust **508 passed**, 0 failed, 63 targets; web **42 passed**; shipped assets asserted absent in the disposable copy | `/tmp/opencode/pkg-batch4-asset-free-driver.log`, `/tmp/opencode/pkg-batch4-asset-free-rust.log`, `/tmp/opencode/pkg-batch4-asset-free-web.log` |
| `bash scripts/check-examples.sh` (`TJ_OFFLINE=1`) | optional Rust **211 passed** (config+API **152**, manager **59**) + local SDLC **2** (**213** Rust total); web examples **11 passed** | `/tmp/opencode/pkg-batch4-check-examples.log` |
| Web unit / production build | **42 passed** / pass (6,132.54 kB single file, 2,263.61 kB gzip) | `/tmp/opencode/pkg-batch4-web-unit.log`, `/tmp/opencode/pkg-batch4-web-build.log` |
| Mocked / real-backend browser | **8** / **9 passed** with the documented `LD_LIBRARY_PATH` | `/tmp/opencode/pkg-batch4-web-e2e-mocked.log`, `/tmp/opencode/pkg-batch4-web-e2e-backend.log` |
| `bash scripts/check-no-business-literals.sh` / `cargo fmt --all -- --check` / `git diff --check` | pass / clean / clean | `/tmp/opencode/pkg-batch4-guard.log`, `/tmp/opencode/pkg-batch4-fmt.log`, `/tmp/opencode/pkg-batch4-diff-check.log` |
| Versioned package build (`scripts/package-release.sh`) | pass; 48 files including **40 `workflows/` files / 4 packages**; no `config/local`, `.env*`, `.local`, DB, log or symlink; `SHA256SUMS` verified | `/tmp/opencode/pkg-batch4-package-build.log`, `/tmp/opencode/pkg-batch4-package-audit.log`, `/tmp/opencode/pkg-batch4-sha-check.log` |
| Package smoke `all` (empty + shipped + synthetic) | all passed; empty readiness `degraded` with **0 workflows**; shipped readiness `ready` with **4** catalogs (`source_kind: package`, non-zero `asset_count`); synthetic with **1** | `/tmp/opencode/pkg-batch4-smoke.log`, `/tmp/opencode/package-smoke-{empty,shipped,synthetic}.log` |
| Local SDLC original hashes | `a88bed13195f925f9af01169ae9b438e3e161cdbc05257443569a25530f5d34f` and `58372bf57846ea61720bbdc1c882f5ab383cedffda84741d9e36ca9dcf354a59` unchanged | `/tmp/opencode/sdlc-regression.log` |

The shipped and local runs are scripted/local only: the browser journeys use a
local scripted model, the SDLC regression uses a disposable bare Git remote and
local Forgejo-protocol fixture, and the package smokes use a dummy provider key.
No live provider, forge, CI/CD, deployment, or external validation is claimed.
The older J4/J5 and audit tables below are retained as dated historical records.

## Dependency policy

Commit both `Cargo.lock` and `webui/package-lock.json`. CI uses `--locked` and
`npm ci`; neither is a dependency-update command. The two direct ADK dependencies
also pin the full Git revision `1ed3c1a6e46b506b75a445bad86a3bd82171e7d9`, matching
the pre-existing lockfile's tested revision. Updates require an explicit manifest
revision change, regenerated lockfile, and the check suite; upstream HEAD is not
the release dependency. Toolchain and Node changes likewise update the documented
baseline and CI rather than silently following a moving release.

## CI asset-free dependency bootstrap (2026-10-23)

`core-asset-free` deliberately keeps the disposable Rust suite offline. A fresh
GitHub runner has no Cargo registry/Git cache, so the job now performs exactly
this order:

1. `cargo fetch --locked` in the real checkout with a job-local
   `CARGO_HOME=${{ runner.temp }}/cargo-home`. This is the only Cargo
   dependency download step and it uses the committed lockfile.
2. `TJ_ASSET_FREE_CARGO_HOME=${{ runner.temp }}/cargo-home bash scripts/check-asset-free.sh`.

The script copies the checkout, removes shipped assets in the copy, and runs
`cargo test --workspace --locked --offline` against the fetched cache. Its
`cargo fetch --locked --offline` preflight aborts with an actionable
`Run 'cargo fetch --locked'` message if the cache is missing, rather than
attempting a long failed build. Asset removal is unchanged and offline remains
the default; `TJ_ASSET_FREE_OFFLINE=0` is a local opt-in to online Cargo and CI
never sets it.

Local cold-cache reproduction on 2026-10-23 used a fresh
`CARGO_HOME=/tmp/opencode/j1-cold-cargo`:

```sh
CARGO_HOME=/tmp/opencode/j1-cold-cargo cargo fetch --locked
TJ_ASSET_FREE_CARGO_HOME=/tmp/opencode/j1-cold-cargo \
  TJ_ASSET_FREE_LOG_DIR=/tmp/opencode/j1-asset-free-cold \
  bash scripts/check-asset-free.sh
```

The fetch passed and populated a 485 MB cache (69 MB Git + 417 MB registry).
The disposable asset-free suite then passed **486 Rust / 0 failed / 61 result
targets** and **42 web tests**. The old empty-cache probe still fails closed
(exit 101, cannot load `adk-core` offline); that is the regression this bootstrap
fixes. Logs: `/tmp/opencode/j1-cold-fetch.log`,
`/tmp/opencode/j1-empty-probe.log`, `/tmp/opencode/j1-script-preflight.out`,
`/tmp/opencode/j1-asset-free-cold/`.

## Historical fresh core matrix (J4, 2026-10-23; superseded by Batch 4 above)

J4 reran the core layer at that time with `--locked` and
`--offline`; default/legacy/ops/all-features used
`CARGO_HOME=$HOME/.cargo`, while asset-free used the complete local J1
cache via the script override. Counts are actual:

| Check | Result | Log |
|---|---|---|
| `cargo test --workspace --locked --offline` | **492 passed**, 0 failed, 61 result targets | `/tmp/opencode/j4-default-rust.log` |
| `bash scripts/check-asset-free.sh` | Rust **492 passed**, 0 failed, 61 targets; web **42 passed**; assets asserted absent in the disposable copy | `/tmp/opencode/j4-asset-free-driver.log`, `/tmp/opencode/j4-asset-free/asset-free-{rust,web,guard,fetch}.log` |
| `bash scripts/check-examples.sh` (with `TJ_OFFLINE=1`) | optional Rust **201 passed**, 0 failed, 26 targets (140 config+API, 59 manager, 2 local SDLC) + web examples **11 passed** | `/tmp/opencode/j4-check-examples.log`, `/tmp/opencode/j4-check-results/` |
| `scripts/check-sdlc-regression.sh` via `check-examples.sh` | shape **1** + operational **1** passed; original hash unchanged | `/tmp/opencode/j4-sdlc-regression/sdlc-regression.log` |
| `cargo test -p tj-manager --features legacy-compat-tests ... legacy_v1_adapter` | **2 passed** | `/tmp/opencode/j4-legacy.log` |
| `cargo test -p tj-ops --lib` | **9 passed** | `/tmp/opencode/j4-ops-lib.log` |
| `cargo check --workspace --all-features --locked --offline` | pass | `/tmp/opencode/j4-all-features.log` |
| `check-no-business-literals.sh` / `cargo fmt --all -- --check` / `git diff --check` | pass / clean / clean | `/tmp/opencode/j4-guard.log`, `/tmp/opencode/j4-fmt.log`, `/tmp/opencode/j4-diff-check.log` |

Notes: the default count grew from the historical 486 because J3 added six
plan-less publication tests. The asset-free run used the complete local J1 Cargo
cache (`/tmp/opencode/j1-cold-cargo`) via `TJ_ASSET_FREE_CARGO_HOME`; the local
`$CARGO_HOME` cache lacks the locked `wasm-bindgen` archives, so its
offline `cargo fetch` preflight correctly fails closed. No network fetch was
made. The default/asset-free run has one pre-existing test-code warning (unused
`tree`, `capability_pipeline_harness.rs:537`). The original/derived local SDLC
hashes were unchanged. J5 freshly recorded web unit **42**, production build,
mocked **8** and real-backend **9** browser journeys, and corrected package
smoke/sha checks; the first J5 package leaked tracked `.local` runtime
placeholders and was rebuilt after a narrow `tj-ops` exclusion fix. Full details
are in the fresh matrix above and the audit matrix below.

## Independent audit check matrix (2026-10-23 audit report; core rows superseded by J4, web/package by J5)

The counts below are what the 2026-10-23 independent-audit logs recorded. The J1
session freshly reproduced the cold-cache fetch and the asset-free **486 Rust +
42 web** suite (see above); the J2 reconciliation session edited documentation
only and did not rerun these rows. J4 replaced the core rows and J5 refreshed the
web/package rows at that time. **The 2026-10-25 Batch 4 packaged-workflow matrix
above now supersedes all of them.** Treat the table below as historical reported
evidence, not current proof.

The default/core layer is **asset-free**. The optional example layer may read
shipped `workflows/` packages and `examples/`; the local SDLC layer is an
additional explicit opt-in. Do not run the optional layer as a substitute for
the core proof.

```sh
# Core engine/unit proof in a disposable copy with all workflow/agent/schema/
# local/example assets removed. Never modifies the real checkout.
bash scripts/check-asset-free.sh

# Optional shipped-example contracts (default suite does not need them)
bash scripts/check-examples.sh

# Local SDLC operational regression (ignored derived graph, scripted model,
# disposable bare Git remote + local HTTP Forgejo protocol). Missing assets are
# reported BLOCKED and exit non-zero.
bash scripts/check-sdlc-regression.sh

# Frozen v1 compatibility (inline historical bundle, never shipped assets)
cargo test -p tj-manager --features legacy-compat-tests --locked --test legacy_v1_adapter

# Versioned package + three disposable smoke modes (empty, shipped, synthetic)
bash scripts/package-release.sh
bash scripts/package-smoke.sh /tmp/opencode/turbojet-package/turbojet-1.0.0 all
```

Packaging policy is explicit: `config/local/**`, `.env*`, nested `.local/**`
tool-home state, symlinks, VCS/tool caches and runtime DB/workspace/log/test-output
paths are excluded regardless of Git ignore state. The four self-contained
packages under `workflows/` are included by policy but are not required: the
empty smoke starts with an empty registry and actionable workflow guidance, the
shipped smoke imports the packaged catalogs with their registry assets, and the
synthetic smoke injects one exact package and proves catalog import plus
project/lane pinning.

| Check | Result | Log |
|---|---|---|
| Default Rust workspace | **486 passed**, 0 failed, 61 result targets | `/tmp/opencode/audit-default-rust.log` |
| Disposable asset-free copy | Rust **486 passed**; web **42 passed**; asset absence asserted | `/tmp/opencode/audit-asset-free-driver.log`, `/tmp/opencode/asset-free-rust.log`, `/tmp/opencode/asset-free-web.log` |
| Optional examples + local SDLC | Rust **195 passed**; web examples **11 passed** | `/tmp/opencode/audit-check-examples.log` |
| Local SDLC operational regression | shape **1** + operational **1** passed; original hash unchanged | `/tmp/opencode/audit-check-examples.log`, `/tmp/opencode/sdlc-regression.log` |
| Legacy v1 adapter | **2 passed** | `/tmp/opencode/audit-legacy.log` |
| Package/backup unit suite | **9 passed** | `/tmp/opencode/audit-ops-lib.log` |
| Web unit / production build (J5 fresh) | **42 passed** / pass | `/tmp/opencode/j5/web-unit.log`, `/tmp/opencode/j5/web-build.log` |
| Mocked / real-backend browser (J5 fresh) | **8** / **9 passed** | `/tmp/opencode/j5/web-e2e-mocked.log`, `/tmp/opencode/j5/web-e2e-backend.log` |
| Empty + synthetic package smoke (J5 fresh) | both passed; no `config/local`, `.local`, `.env*`, DB/log or symlink in package | `/tmp/opencode/j5-fix-canonical-smoke.log`, `/tmp/opencode/j5-fix-package-audit.log` |

These are the 2026-10-23 audit report's working-tree counts, not a release and
not rerun by the J1/J2 sessions. J4/J5 have since rerun the core/web/package
matrix; the fresh results are above. The audit-reported
F1–F14 matrix and residual gaps are in the local workflow-decoupling
final-review record.

## Historical batch check records (superseded by the independent audit above)

Default suites are now isolated from shipped workflows. Run:

```sh
CARGO_HOME="$HOME/.cargo" cargo test --workspace --locked --offline
(cd webui && npm test)
bash scripts/check-no-business-literals.sh
bash scripts/check-asset-free.sh          # disposable copy; add --keep
bash scripts/checks.sh examples            # optional shipped-example layer
bash scripts/check-examples.sh             # green at Batch 5; includes opt-in local SDLC regression
```

`check-asset-free.sh` copies the checkout with `rsync` (never modifying it),
removes `workflows/`, `config/local/`, legacy top-level graph files, the optional
test-package fixtures, and `examples/` **only in the copy**, reuses the installed `webui/node_modules`
read-only, and runs the locked offline Rust suite plus the web unit suite. It
uses `$HOME/.cargo` when the exported `CARGO_HOME` lacks the git cache; override
with `TJ_ASSET_FREE_CARGO_HOME` and `TJ_ASSET_FREE_LOG_DIR`.

| Check | Result | Log |
|---|---|---|
| Default Rust workspace | **449 passed**, 0 failed, 56 targets | `/tmp/opencode/batch1-default-rust.log` |
| Asset-free disposable copy | **449 passed**, 0 failed; web **39 passed** | `/tmp/opencode/asset-free-rust.log`, `/tmp/opencode/asset-free-web.log` |
| No-business-literal guard | pass on default test layer | `/tmp/opencode/asset-free-guard.log` |
| Optional web examples | **11 passed** (4 files) | `/tmp/opencode/batch1-web-examples.log` |
| `cargo check --workspace --all-features --locked --offline` | pass | `/tmp/opencode/batch1-all-features.log` |
| Optional Rust examples | API **11 passed** across 5 harnesses; manager **42 passed**; `tj-config` **4 known failures** | `/tmp/opencode/batch1-rust-examples.log` |
| Mocked browser | build + `playwright --list` (8 cases) pass; launch blocked by missing host `libnspr4.so` | `/tmp/opencode/batch1-web-e2e.log` |

Known optional blockers listed in this historical Batch 1 record were resolved by
the Batch 4 example/local-SDLC migration. Current optional and local-SDLC
results are in the 2026-10-25 packaged-workflow matrix above
(`check-examples.sh` green; local regression is opt-in). The asset-free proof ran with `config/local/full-sdlc.smg.json` present in the
real checkout and removed only in the disposable copy.

## Batch 2 empty-registry and registry-catalog baseline (2026-10-22)

At that baseline, production startup needed no shipped/default graph. Developer
and self-hosted configs declared `workflow_sources: []`; explicit file/directory
sources were validated before registry writes. The current shipped templates
load `workflows/`, and an empty source list remains supported. The default
suites remain asset-free and synthetic.

| Check | Result | Log |
|---|---|---|
| Default Rust workspace | **457 passed**, 0 failed, 58 targets | `/tmp/opencode/batch2-default-rust.log` |
| Disposable asset-free copy | **457 passed**, 0 failed; web **40 passed** | `/tmp/opencode/asset-free-rust.log`, `/tmp/opencode/asset-free-web.log` |
| Web unit | **40 passed** (6 files) | `/tmp/opencode/batch2-web-unit.log` |
| Production build | pass (6,132 kB single file / 2,264 kB gzip) | `/tmp/opencode/batch2-web-build.log` |
| Mocked browser | **8 passed**, actually launched with `LD_LIBRARY_PATH=/tmp/opencode/turbojet-browser-deps/root/usr/lib/x86_64-linux-gnu` | `/tmp/opencode/batch2-web-e2e.log` |
| Business-literal guard | pass | `/tmp/opencode/batch2-guard.log` |
| All-features compile | pass | `/tmp/opencode/batch2-all-features.log` |

Batch 2 acceptance coverage is
`crates/tj-config/tests/workflow_sources_test.rs` and
`crates/tj-api/tests/registry_catalog_harness.rs` (zero-workflow HTTP/readiness,
empty catalog, 409 before workspace, arbitrary catalog import, exact pin,
source removed + restart, registry-only run admission, actionable missing pin,
cross-project pin 403). The default lifecycle harness now requires the exact
artifact identity for the temporary safe artifact-bound gate.

**Historical Batch 2 residual (resolved by Batch 4/5; preserved as dated
evidence):** optional shipped-example suites still needed their Batch 4
catalog/capability migration. `scripts/check-examples.sh` at that time exited 1
(12 optional API journey failures and 3 optional config distribution failures);
manager feature tests (42) and web example tests (11) passed. They failed closed
and were excluded from that default evidence. Capability contract/bundle v2 and
normalized run gate kind were Batch 3 work. Log:
`/tmp/opencode/batch2-examples.log`. The Batch 4/5 implementation later migrated
these suites; see the local Batch 5 record in the workflow-decoupling plan.

## TJ-023–TJ-040 local verification (2026-10-22)

Isolated caches only:

```sh
CARGO_HOME=/tmp/opencode/turbojet-clean-cargo \
CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target \
  cargo test --workspace --offline

(cd webui && npm test -- --run)
(cd webui && npm run build)

# Real backend + real UI + local scripted model, disposable state:
(cd webui && TJ_API_BINARY=/tmp/opencode/turbojet-clean-target/debug/tj-api \
  LD_LIBRARY_PATH=/tmp/opencode/turbojet-browser-deps/root/usr/lib/x86_64-linux-gnu \
  npx playwright test --config playwright.backend.config.ts)
```

Results in this working tree:

| Check | Result | Log |
|---|---|---|
| Rust workspace | **483 passed**, 0 failed, no warnings | `/tmp/opencode/generic-workspace-test.log` |
| UI unit | **50 passed** | `/tmp/opencode/generic-web-unit.log` |
| Production build | 6,131.60 kB single file / 2,263.34 kB gzip | `/tmp/opencode/generic-web-build.log` |
| Mocked browser | **8 passed** | `/tmp/opencode/generic-browser.log` |
| Real-backend scripted journeys | **9 passed** (8 Discovery + 1 Planning) | `/tmp/opencode/tj-final-engineering-e2e-backend2.log` |
| Delivery contracts/API drill | **1 integrated API+SQLite+HTTP-adapter drill passed**; pure state/HMAC/adapter + DB regressions included in workspace count | `/tmp/opencode/tj-final-engineering-backup-rust6.log` |
| `tj-ops` backup/restore/package | **9 passed** (offline lock, consistency, external-key, failed-upgrade restore) | `/tmp/opencode/tj-final-engineering-backup-rust6.log` |
| SIGTERM process smoke | **1 passed** (`tj-api` exits cleanly) | `cargo test -p tj-api --test sigterm` |
| Package drill | local `turbojet-0.1.0` with checksums; `.env`/symlinks excluded, not installed/published | `/tmp/opencode/tj025-package.log` |
| Packaged-binary smoke | disposable copy served built UI, readiness `ready`, clean SIGTERM | `/tmp/opencode/tj025-package-smoke.log` |

The nine real-backend browser cases cover the eight Discovery journeys plus the
Planning journey: exact packet approval, sequential Planning, bounded evidence
rework, reviewer plan edit/stale approval conflict, exact plan+packet approval
and ready-task selection. The Discovery cases cover happy path/support export,
contradictory approval block, rejection/rework, browser reconnect plus delayed
reply, cancel without another model call, restart plus explicit retry,
transient retry versus credential failure, and question-budget exhaustion. They use the actual API/Manager/Flow/Stage/SQLite/workspace and a
local scripted OpenAI-compatible model. **They are not live supported-model or external pilot
proof.** The prepared live scenario remains BLOCKED in
`tests/qa/scenarios/discovery-live.md`; TJ-033 external pilot validation is
BLOCKED in `tests/qa/scenarios/planning-pilot.md`.


## TJ-034–TJ-040 local Engineering and delivery checks (2026-10-22)

Same isolated caches as above. The new boundary suite runs against the real
Linux kernel, not a simulated sandbox:

```sh
CARGO_HOME=/tmp/opencode/turbojet-clean-cargo \
CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target \
  cargo test -p tj-tools --test isolation_boundary --offline

CARGO_HOME=/tmp/opencode/turbojet-clean-cargo \
CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target \
  cargo test -p tj-manager --test publication_harness --offline
```

| Check | Result |
|---|---|
| Landlock/seccomp boundary | **8 passed**: host/credential blocking, symlink escape denial, TCP + UDP/AF_UNIX/setsid denial, env/limits, 1 MiB output cap, background process, missing workspace fail-closed |
| Publication state machine | **4 DB passed**: unique operation identity, failed-push cannot succeed, unknown reconciliation, PR requires pushed branch |
| Local disposable forge | **9 passed**: bugfix success, feature scope refusal, failing test, model-only test claim refusal, cancel retention, rejected push, failed PR, lost response without duplicate, restart/unknown-push reconciliation |
| SSH/key contracts | **5 passed**: restrictive atomic files and cleanup, missing host identity fail-closed, absent key, symlink refusal, URL detection |
| Manager selection/cancel regressions | **2 passed**: arbitrary selection cannot start Engineering; cancelled uncommitted workspace retained |

The disposable forge is a local bare Git repository plus a local HTTP server
implementing the Forgejo PR subset. The delivery drill uses another disposable
HTTP server implementing the Forgejo Actions dispatch/run-list subset and sends
HMAC-signed callbacks through the real API. **No real SSH/Forgejo clone, push,
PR, Actions run, deployment or rollback is tested or claimed here.** On this host unprivileged namespaces/bwrap are
unavailable; the active boundary is Landlock ABI 8 + seccomp. If a host lacks
the prerequisite, shell execution fails closed and readiness reports
`execution_isolation` BLOCKED.

## Final independent acceptance pass (2026-10-22)

The final audit reran the full Rust workspace after fixing the seams below
(`CARGO_HOME=/tmp/opencode/turbojet-clean-cargo`,
`CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target`):

| Check | Result | Log |
|---|---|---|
| Rust workspace | **483 passed**, 0 failed, no warnings | `/tmp/opencode/generic-workspace-test.log` |
| `tj-tools` boundary | 74 unit + **8 live boundary** passed | `/tmp/opencode/tj-final-engineering-backup-rust6.log` |
| `tj-manager` publication harness | **9 passed** | included above |
| `tj-ops` restore/package | **9 passed** | included above |
| `tj-api` API/harness/sigterm | 22 unit + 1 delivery + 2 discovery + 6 lifecycle + 2 planning + 2 engineering + 1 SIGTERM passed | `/tmp/opencode/tj-final-engineering-backup-rust6.log` |

No web source changed in this pass. The final rerun still passed **46** UI unit
tests, the production build, **7** mocked browser cases and **9** real-backend
scripted browser journeys (`/tmp/opencode/tj-final-engineering-ui-unit.log`,
`/tmp/opencode/tj-final-engineering-ui-build.log`,
`/tmp/opencode/tj-final-engineering-e2e-mocked.log`,
`/tmp/opencode/tj-final-engineering-e2e-backend2.log`).

The pass added a full API→Manager→Flow→Stage→real-shell→publication Engineering
harness (with a failing-test/no-push journey and chained delivery dispatch),
enforced offline/quiesced backup under the exclusive instance lock, and fixed the
task-worktree, pre-commit changed-file/scope, ADC/SMG stage-log, per-stage output
directory and API `git_provider` wiring discovered by that harness. Findings and
residual gaps are in the local integration final-review record.

## Browser and live prerequisites

`npm run e2e` in `webui/` builds production assets before testing and starts its
own preview server on 127.0.0.1:8099. It refuses to reuse an unrelated running
server. Its default config selects only `webui.spec.ts`. Every API request in
that suite must have a fixture; unmatched requests fail the test and cannot reach
a real backend.

If Chromium cannot launch, the preflight prints **BLOCKED**, the installation
command, and the launch error, then exits with status 2 before application
assertions. This is an unavailable prerequisite, not a passed/skipped test suite.
Run `npx playwright install --with-deps chromium` from `webui/` to install the
browser matching the lockfile and its required libraries.

The real-backend **scripted** suite is separate from both mocked and live:

```sh
cd webui
npm run e2e:backend
```

It builds the UI and `tj-api`, then Playwright starts a disposable backend plus
the local model fixture for each case. It never contacts a live provider. Set
`TJ_API_BINARY`/`TJ_WEBUI_PATH` when using isolated caches. Missing
browser/binary prerequisites are reported before assertions, not as passes.

The separate opt-in live command is:

```sh
cd webui
TJ_E2E_USER=... TJ_E2E_PASS=... TJ_E2E_LIVE_DISCOVERY=1 npm run e2e:live
```

Without `TJ_E2E_LIVE_DISCOVERY=1` the prepared Discovery live journey is
reported as BLOCKED and does not claim a pass.

It requires an already-running dedicated test API at `http://127.0.0.1:8100` (or
`TJ_E2E_API_URL`) and explicit test credentials. Missing credentials/unhealthy API
are reported as **BLOCKED**, exit 2. The live smoke reads the dashboard; full
model/forge scenarios remain separately operated QA and may incur costs. Default
checks/CI never run live/provider/forge scenarios or invent credentials.

## October integration verification

The 2026-10-22 TJ-019–022 slice used the isolated caches named below:
`cargo test --workspace --locked --offline` passed **380** tests with no
warnings; `npm test -- --run` passed **38** unit tests; `npm run build` passed;
and `npm run e2e` passed **7** mocked browser cases with
`LD_LIBRARY_PATH=/tmp/opencode/turbojet-browser-deps/root/usr/lib/x86_64-linux-gnu`.
The integrated harness `crates/tj-api/tests/lifecycle_harness.rs` uses a local
scripted OpenAI-compatible server, a disposable SQLite database/workspace and
the real API/Manager/Flow/Stage chain; it makes no paid or live call. Logs:
`/tmp/opencode/tj019-022-workspace-final5.log`, `/tmp/opencode/tj019-ui-unit5.log`,
`/tmp/opencode/tj019-ui-build2.log`, and
`/tmp/opencode/tj019-e2e3.log`.

Executed on Ubuntu 24.04 with Rust 1.98.0, Node 22.23.2, and npm 10.9.8:

- Downloaded dependencies from public registries/Git with an initially empty
  `CARGO_HOME=/tmp/opencode/turbojet-clean-cargo`; no normal user Cargo cache was
  used. The manifest pin changes ADK source identity, not its revision/version.
- Built into a new `CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target` and ran
  `bash scripts/checks.sh rust`: **293 passed**, no warnings. The first isolated
  run found one fixture assuming `/tmp/<dir>/../../etc/passwd` existed; it now
  creates its own outside-workspace file and works at arbitrary temporary depth.
- `bash scripts/checks.sh web`: clean **431-package npm install**, **29 unit tests**
  passed, TypeScript/Vite production build passed. After updating stale browser
  navigation/activity fixtures, `npm exec playwright test`: **5 passed**.
- Browser preflight without the host's missing libraries: **BLOCKED, exit 2**.
  Live preflight without explicit credentials: **BLOCKED, exit 2**, no live calls.
- `git diff --check` passed. The GitHub Actions workflow is newly added; a remote
  CI run and a live model/forge run are not claimed.

This restricted host has no passwordless system-package installation. For the
executed browser checks, official Ubuntu 24.04 `libnspr4_4.35-1.1build1` and
`libnss3_3.98-1ubuntu0.2` packages were SHA-256 verified and extracted under
`/tmp/opencode/turbojet-browser-deps`; tests used
`LD_LIBRARY_PATH=/tmp/opencode/turbojet-browser-deps/root/usr/lib/x86_64-linux-gnu`.
This was a local prerequisite workaround; CI uses Playwright's supported
`--with-deps` installation. No system libraries were changed.

Local logs: `/tmp/opencode/turbojet-clean-fetch.log`,
`/tmp/opencode/turbojet-repro-rust-final.log`,
`/tmp/opencode/turbojet-repro-web.log`, and
`/tmp/opencode/turbojet-repro-browser.log`. Browser reports are generated under
`webui/playwright-report/`. The commands/counts here are the durable evidence.

Two observed unit-test warnings were addressed: workflow graph/accordion siblings
now have distinct React keys, and the workflow-detail fixture supplies its lanes
query response. These were an actual rendering-key collision and an incomplete
fixture respectively, not warnings to suppress globally.

The browser fixtures now follow the approval panel and Chat tab explicitly,
provide activity-log responses, and reject unexpected API requests. Assertions
still verify approvals, structured rejection causes, workflow-based run creation,
and exact structured question-answer payloads.

## 2026-10-22 Engineering E2E and offline backup pass

- `cargo test --workspace --locked --offline` with isolated
  `CARGO_HOME=/tmp/opencode/turbojet-clean-cargo` and
  `CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target`: **474 passed**, 0
  failed, no warnings (`/tmp/opencode/tj-final-engineering-backup-rust6.log`).
- `cargo test -p tj-api --test engineering_harness`: **2 passed**
  (`/tmp/opencode/tj-final-engineering-e2e-final2.log`). Success path: Discovery
  packet + Planning plan approved through the API, T-1 selected through the API,
  real sandboxed shell writes/executes code and tests, production Manager
  publishes verified commit/remote SHA and one draft PR to a disposable bare Git
  remote + local Forgejo HTTP fixture, then an explicit staging delivery dispatch
  binds that publication. Failure path: real non-zero test routes back to
  implementation, run ends failed with no branch/PR.
- `cargo test -p tj-ops`: **9 passed** (`offline_exclusive_lock` refusal,
  lock retention/release, failure cleanup, point-in-time restore, external key,
  failed upgrade, package layout). `cargo test -p tj-manager --test
  publication_harness`: **9 passed**. `cargo test -p tj-tools --test
  isolation_boundary`: **8 passed**.
- Web source was unchanged; the final rerun passed 46 unit tests, the production
  build, 7 mocked Playwright cases and 9 real-backend scripted browser journeys
  (`/tmp/opencode/tj-final-engineering-ui-unit.log`,
  `/tmp/opencode/tj-final-engineering-ui-build.log`,
  `/tmp/opencode/tj-final-engineering-e2e-mocked.log`,
  `/tmp/opencode/tj-final-engineering-e2e-backend2.log`).
- `git diff --check` passed. No commit, push, deploy, production operation, live
  provider/forge/CI call, or paid effect was performed.

## 2026-10-22 generic repository-free workflow rerun

- Full locked Rust workspace: **483 passed**, 0 failed
  (`/tmp/opencode/generic-workspace-test.log`). Three new integrated journeys
  are in `crates/tj-api/tests/generic_workflows_harness.rs`:
  `cargo test -p tj-api --test generic_workflows_harness` → **4 passed**
  (`/tmp/opencode/generic-harness.log`). They execute the real API, Manager,
  Flow, Stage, SQLite and run workspace with a scripted local model: checker
  correction re-enters act, a durable user answer plus rejection/revision
  approves the exact output artifact ID/hash, real citations complete while an
  invented citation fails and a missing-evidence loop still completes from
  supplied documents. All assert no `.git`/PR effects.
- Config route/schema tests plus `crates/tj-config/tests/generic_workflows_test.rs`
  (3 tests) cover every shipped graph, exact limits, confined tools, artifact
  declarations and documented loops; no config or schema was silently ignored.
- UI: **50 unit tests passed** (`/tmp/opencode/generic-web-unit.log`),
  production build passed at **6,131.60 kB / 2,263.34 kB gzip**
  (`/tmp/opencode/generic-web-build.log`), and **8 mocked Playwright cases
  passed** with
  `LD_LIBRARY_PATH=/tmp/opencode/turbojet-browser-deps/root/usr/lib/x86_64-linux-gnu`
  (`/tmp/opencode/generic-browser.log`). The new case opens a generic review
  gate, displays the bound artifact, and posts its exact ID/hash/stage.
- All commands used `CARGO_HOME=/tmp/opencode/turbojet-clean-cargo` and
  `CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target`. The work remains
  uncommitted for the owner's later squash; no live provider, Git, forge, SSH,
  deployment or external validation was used.

## Batch 4 capability-module baseline (2026-10-22)

All declared optional capability modules now dispatch from the frozen v2
contract. The manager/API/UI do not infer behavior from a flow, stage, gate,
profile, catalog id or file name. The isolated v1 adapter is the only historical
name mapper. Shipped examples carry catalog + capability declarations; the
original `config/local/full-sdlc.smg.json` is unchanged and an ignored derived
copy is produced/imported explicitly.

| Check | Result | Log |
|---|---|---|
| Default Rust workspace | **478 passed**, 0 failed | `/tmp/opencode/batch4-default-final3.log` |
| Disposable asset-free copy | Rust **478 passed**, web **40 passed** | `/tmp/opencode/asset-free-rust.log`, `/tmp/opencode/asset-free-web.log` |
| Optional `scripts/checks.sh examples` | Rust **184 passed** (config+API 127, manager 57), web **11 passed** | `/tmp/opencode/batch4-check-examples3.log` |
| Legacy v1 adapter | **2 passed** | `/tmp/opencode/batch4-legacy.log` |
| All-features compile | pass | `/tmp/opencode/batch4-all-features.log` |
| Web unit / production build | **40 passed** / pass | `/tmp/opencode/batch4-web-unit2.log`, `/tmp/opencode/batch4-web-build2.log` |
| Mocked browser | **8 passed** | `/tmp/opencode/batch4-web-e2e2.log` |
| Real-backend scripted browser | **9 passed** | `/tmp/opencode/batch4-web-e2e-backend2.log` |
| Business-literal guard / fmt | pass | `/tmp/opencode/batch4-guard2.log`, `/tmp/opencode/batch4-fmt.log` |

New default proof: `crates/tj-api/tests/capability_pipeline_harness.rs` runs a
renamed `collect → approve → shape → choose → deliver-code` pipeline through the
real API/Manager/Flow/Stage/SQLite path (packet, plan, selection, inputs, local
citations) and a flow literally named `engineering` with no publication (no
Git, remote or selection effect). Optional local SDLC validation checks the
derived copy differs only by `catalog`/`capabilities` and recomputes the original
hash `a88bed13195f925f9af01169ae9b438e3e161cdbc05257443569a25530f5d34f`.

Operator local-SDLC migration:

```sh
bash scripts/import-local-sdlc.sh derive
bash scripts/import-local-sdlc.sh configure   # explicit workflow_sources entry
# restart the service, then:
TJ_API_BASE=http://127.0.0.1:8100 TJ_API_TOKEN=... \
  bash scripts/import-local-sdlc.sh import-pin --lane <lane-id>
```

**Historical Batch 4 hand-off (2026-10-22; superseded by Batch 5, 2026-10-23):**
at that point Batch 5 work remained: Chief rename, packaging
excludes/smokes, optional-example CI split and the final acceptance matrix. It
was implemented in the working tree on 2026-10-23; see the
local Batch 5 record (workflow-decoupling plan).
Fresh final counts are recorded in the fresh matrix above (J4 core, J5
web/package).
