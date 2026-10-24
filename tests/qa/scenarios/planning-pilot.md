# Planning pilot scenario (M4) — local scripted evidence, external validation BLOCKED

**Status:** LOCAL SCRIPTED PASS · EXTERNAL PILOT **BLOCKED**\
**Task:** TJ-033\
**Graph:** runtime-synthesized neutral planning-capability package (`webui/tests/e2e/neutral-planning-package.ts`); no tracked flow fixture\
**Backend graph:** the same disposable package, generated in test state; the operator-local, Git-ignored `config/local/full-sdlc.package/` remains an opt-in regression and is absent in a fresh public checkout

> This scenario is a local contract drill. It is **not** feedback from real
> engineering teams and must never be reported as external pilot validation.
> TJ-033 remains blocked until two real pilot teams/engineers use exported
> plans for real work and their recorded evidence is published.

## What is exercised locally

1. A Discovery requirements packet is generated and approved at
   `collect-gate` with the exact packet revision/hash.
2. Planning executes sequentially:
   `survey → outline → sketch → align → plan → check` and then the
   Manager's deterministic plan validation. No stage is skipped.
3. The accepted input is exactly the approved packet revision. A plan that
   claims another packet revision is rejected before persistence.
4. A planning-gate rejection performs one bounded rework and all required
   Planning stages execute again.
5. A reviewer edit creates an immutable plan revision bound to the same
   approved packet, rebinds the open gate and bumps the run state version. An
   approval prepared for the previous plan/hash conflicts with `409`.
6. The exact plan+packet approval is recorded. Planning never commits, pushes
   or opens a pull request.
7. The ready-task contract lists `T-1` as ready and `T-2` as blocked by its
   dependency. Selecting `T-1` after approval records the plan/packet/task
   identity plus criteria and provenance; blocked or stale selections fail.

## Automated local evidence

| Check | Command | Evidence |
|---|---|---|
| Real API → Manager → Flow → Stage planning journey | `cargo test -p tj-api --test planning_harness` | 2 integrated cases: full bounded-rework/stale-edit/selection journey and pre-gate deterministic rejection |
| Plan schema/trace/DAG/readiness/export | `cargo test -p tj-plan` | 9 crate tests |
| Immutable revisions, gate binding, reviews, selections | `cargo test -p tj-db plans::` | 4 DB tests |
| Browser journey | `LD_LIBRARY_PATH=... TJ_API_BINARY=... npx playwright test --config playwright.backend.config.ts planning-journey` | Real backend + built UI + local scripted model; no live provider |
| Planning read-only and graph contract | `cargo test -p tj-config smg_route_schemas_test` | Sequential stages, validation before gate, no bash/edit/write |
| UI review/diff/evidence/blocked/selection | `cd webui && npm test -- --run` | Planning unit tests |

The scripted browser fixture is `scripts/discovery_scripted_model.py`; all
model calls are local and deterministic.

## External pilot protocol (BLOCKED — do not fabricate)

The full protocol and templates are in
[`evaluation/planning-pilot/`](../../../evaluation/planning-pilot/README.md).

Required before TJ-033 can be marked complete:

- [ ] Two real pilot teams or engineers are recruited and consent recorded.
- [ ] Each pilot receives an exported plan JSON **and** Markdown carrying the
      exact packet/plan revision and hash from an approved local run.
- [ ] Each pilot uses the plan for real work against its own repository or
      process; no TurboJET agent performs the engineering work for them.
- [ ] Clarifications, prerequisites, invented-scope observations, corrections
      and time/effort are recorded against the packet/plan versions.
- [ ] A named owner reviews the evidence, verifies no unexplained requirement
      loss, and decides whether Planning is released and Engineering is
      authorized or deferred.
- [ ] The owner records blockers to close before M5.

No participant names, scores, feedback, quotes or owner decision may be added
to this file unless they come from a real pilot. Until then every external item
is explicitly **BLOCKED**.
