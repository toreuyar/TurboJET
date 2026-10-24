# Generic repository-free workflows

**Status:** implemented and integrated locally on 2026-10-22; all three templates are labelled **experimental**. No live model, external validation, Git or deployment is claimed. The work remains uncommitted for the owner's later squash.

> **Batch 5 note (2026-10-22):** capability schema v1 and execution bundle v2
> remain the runtime contract; the shipped examples declare catalog ids plus
> capabilities and behavior comes only from that declaration. The canonical
> persona name is **Chief** (the
> legacy config id `chef`, `TURBOJET_AGENT_CHEF_MODEL` and browser `who: "chef"`
> values are accepted only through bounded migration code and tests). Catalog ids
> and labels are arbitrary registry data. The four shipped packages under
> `workflows/` are optional distribution assets loaded by the shipped templates:
> a service with no `workflow_sources` starts empty and never falls back to an
> example or the local SDLC. The ignored local full-SDLC package is an explicit
> operator opt-in (`scripts/import-local-sdlc.sh`); it is never packaged or
> selected by filename.

TurboJET ships three small, general-purpose workflow packages alongside the
supported Discovery package. The experimental full SDLC is operator-local only.
These are ordinary SMG graphs:
the real `API → Manager → Flow → Stage → frozen bundle` runtime executes their
prompts, structured verdicts, artifacts, gates and retries. They are not a
disconnected JSON demo.

Every template:

- is **repository-free** — project creation does not run `git init`/clone and run
  admission does not create a worktree or publish anything;
- grants only the confined `read_file`/`write_file` tools inside the run
  workspace (`bash: false`, `edit: false`, no web/forge/SSH credential);
- ends without a commit, push, PR, merge or deployment;
- uses the configured provider/model and the normal run-wide budgets.

Registered catalog graphs and their lane templates are listed by
`GET /v1/profiles` and the workflow registry (`GET /v1/workflows`). The workflow
viewer draws every stage and route, including feedback loops, and shows the
catalog/capability/maturity metadata.

## The three flows

### 1. Agent loop (`agent-loop`)

```text
plan (interactive, may ask)
  ├─ success ─────────────► act
  └─ needs-input ────────► result
act
  ├─ produced ───────────► check
  └─ needs-input ────────► result
check
  ├─ pass ───────────────► result
  ├─ revise ─────────────► act   (feedback re-enters act)
  └─ needs-input ────────► result
result ── success ─────────► exit
```

`plan` records the deliverable, ordered approach, checkable criteria and honest
unknowns. `act` receives those criteria plus the original request. When `check`
returns `revise`, the next `act` receives the prior draft and the exact
`feedback` array; `feedback_applied` makes the correction auditable. The loop is
bounded by `maxSteps: 12` and `maxIterations: 3`. `needs-input` is explicit and
never replaced by an invented answer.

Examples: summarize supplied notes, turn a rough idea into a proposal, or apply
a small writing transformation.

### 2. Clarify → Produce → Human review (`clarify-produce-review`)

```text
clarify (interactive)
  ├─ question tool ──► durable user answer ──► continue
  └─ success ────────► produce
produce ── produced ───► review-gate
review-gate
  ├─ approved ─────────► completed
  └─ rejected ─────────► clarify (rework with the accepted cause)
```

Clarification asks the user through the durable browser question channel when a
missing choice changes the output. `produce` receives the accepted
clarification plus `__rework.cause` on a rejection and creates the complete
deliverable. The human gate binds the **exact output artifact ID, flow, stage
and SHA-256 content hash** shown to the reviewer. Rejection re-enters the flow
so the artifact is revised; approval records the new identity. Rejections are
bounded at three accepted decisions, after which the run is cancelled.

Examples: write a short launch note after asking for audience/length, or turn a
rough brief into a reviewable proposal.

### 3. Research → Synthesize → Verify (`research-synthesize-verify`)

```text
research ── found ─────────────► synthesize
   ▲  └──── missing ───────────► research (bounded retry)
   │
synthesize ── drafted ─────────► verify
   ▲  └──── missing-evidence ──► research
   │
verify
   ├─ verified ────────────────► completed
   ├─ revise ─────────────────► synthesize
   └─ missing-evidence ───────► research
```

Research lists and reads only `inputs/` in the isolated run workspace. Provided
project documents are copied into that directory at run start (regular files
only, no symlinks or path traversal). Synthesis must attach citations with a
relative `inputs/...` path and an exact excerpt. Verification checks them, and
the Manager independently re-reads every file before the run can complete:

- the path must be relative, remain inside `inputs/`, exist and be readable text;
- the excerpt must appear verbatim in the cited file;
- URLs, absolute paths, `..` escapes and invented files fail the run with
  `citation consistency failed`;
- the research/revise back-edges are bounded by `maxSteps: 10`.

No web search, model memory, repository or external proof is accepted. Missing
evidence loops visibly rather than producing a confident unsupported answer.

Examples: compare supplied option documents, answer a question from provided
notes, or produce a source-backed briefing.

## Setup and examples

The templates are shipped as self-contained packages under `workflows/` and are
loaded by the `workflows` directory source in the default configs. Each package
owns its graph, agents, prompts and schemas:

```text
workflows/agent-loop/
  workflow.smg.json
  agents/{agent-plan,agent-act,agent-check,agent-result}/
  schemas/agent-loop-*.json
workflows/clarify-produce-review/
  workflow.smg.json
  agents/{clarify,produce}/
  schemas/{clarify,produce}.json
workflows/research-synthesize-verify/
  workflow.smg.json
  agents/{research,synthesize,verify}/
  schemas/{research,synthesize,verify-result}.json
examples/generic/{notes,option-a,option-b,idea}.md
```

### UI

1. Open **Runs** (or the first-run setup card) and choose **+ Project**.
2. Pick a workflow in the **Workflow catalog** — the list comes from
   `GET /v1/profiles` and shows label/maturity. If several catalogs have no
   declared default, an explicit choice is required.
3. Leave the repository URL empty unless the selected contract declares a
   repository workspace (the field is hidden otherwise). Its Default lane pins
   the exact matching workflow version.
4. Start a run with a description. For research, first copy supplied documents
   into `<project workspace>/inputs/`; they are copied to the run workspace.
5. Interactive stages show durable questions in the stage panel. Review gates
   show the exact bound output artifact and hash before approval/rejection.
6. **Workflows** lists every registered template and graph; **Use this
   template** creates a matching project without config editing.

Example input setup:

```bash
PROJECT_WS=.turbojet/proj_...      # shown in the project API/UI
mkdir -p "$PROJECT_WS/inputs"
cp examples/generic/option-a.md examples/generic/option-b.md "$PROJECT_WS/inputs/"
```

### API/Chief

```bash
# Discover registered catalog entries and immutable workflow versions
curl -s -H "Authorization: Bearer $TOKEN" localhost:8100/v1/profiles
curl -s -H "Authorization: Bearer $TOKEN" localhost:8100/v1/workflows

# Import/rescan the explicitly configured workflow sources. The request never
# accepts an arbitrary filesystem path; add the file to a configured file or
# directory source, then call import or restart.
curl -s -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  localhost:8100/v1/workflows/import -d '{}'

# Create a repository-free project by catalog id (or exact workflow_id)
curl -s -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  localhost:8100/v1/projects \
  -d '{"name":"Notes summary","catalog_id":"agent-loop"}'

# Start its Default run with the returned project_id/lane_id
curl -s -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  localhost:8100/v1/pipeline/runs \
  -d '{"description":"Summarize examples/generic/notes.md","project_id":"proj_...","lane_id":"lane_..."}'

# Exact generic output for review (artifact + gate binding)
curl -s -H "Authorization: Bearer $TOKEN" \
  localhost:8100/v1/pipeline/runs/run_.../output

# Approve that exact artifact (same fields for rejection, with approved:false
# and a cause). Stale/missing output identity returns 409 before any write.
curl -s -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  localhost:8100/v1/pipeline/runs/run_.../approve \
  -d '{"approved":true,"gate_instance_id":"gate_...","expected_state_version":7,
       "idempotency_key":"...","output_artifact_id":"art_...",
       "output_content_hash":"...","output_stage_name":"produce"}'
```

Chief's `list_flows` returns registered catalog entries with arbitrary catalog
ids, maturity, stages, declared capabilities, gate presence and lane IDs.
`create_project` accepts `workflow_id` (preferred), any registered `catalog_id`,
or the legacy `profile` alias only when it names a registered catalog. There is
no built-in catalog, default graph or filename inference. A local/private graph
is imported through the same explicit `workflow_sources` mechanism as shipped
examples; use the
operator commands in `scripts/import-local-sdlc.sh`.

## Artifacts, limits and errors

- Each stage's immutable run artifact is the validated structured JSON returned
  by `verdict`. Prompts also mirror the readable result to `output.md` in the
  stage output location. Non-interactive stages use the existing validated
  structured-mirror rule when no file is written.
- A workflow without `requirements_packet`/`task_plan` reads or writes no
  packet or plan. Packet/plan production and gate binding are dispatched only
  when the frozen contract declares those modules, using the declared flow,
  stage and gate names. A gate with no declared binding defaults to the latest
  immutable artifact of the reviewed flow.
- Shared SMG limits: agent loop `maxIterations 3 / maxSteps 12 / maxDepth 4`;
  clarify/review `3 / 12 / 4`; research `3 / 10 / 4`. Stage ADC timeout 300 s and
  `max_output_retries: 3`.
- Run-wide budgets, active time, question rounds, model/tool calls and
  cancellation are the same durable contracts as every other profile.
- `revise`/`missing-evidence` route back inside the graph; exhausting
  `maxSteps`/`maxIterations` fails the flow visibly. A malformed structured
  output gets the normal bounded correction, not a fake success.
- Gate rejection re-runs the flow with `__rework`; three accepted rejections
  cancel the run. Generic gates never validate a business packet or plan.
- Citation verification failures fail the run before completion and are visible
  in the `flow_failed`/`run_failed` events. There is no fallback to external
  evidence.

## Verification

`crates/tj-api/tests/generic_workflows_harness.rs` drives the real API, Manager,
Flow, Stage, SQLite and isolated workspace with a scripted local model (four
integrated journeys):

1. **Agent loop:** plan criteria reach act; a `revise` verdict re-enters act with
   the exact feedback; the corrected draft passes; six stage/model turns plus
   the interactive write complete the run with no `.git`/PR.
2. **Clarify/review:** a durable question is answered and reaches the model;
   the first artifact is rejected with a cause; the flow re-enters clarification
   with `__rework`; the revised artifact gets a new ID/hash; exact-output
   approval completes the run and both decisions retain their identity.
3. **Research:** real citations complete; a fake path/excerpt fails with
   `source verification failed`; a `missing-evidence` loop re-enters research
   and synthesis and still completes from real inputs. All runs assert no Git
   effects.
4. **Bounded rejection:** three accepted human rejections cancel the review run
   after exactly three clarify/produce attempts; no fourth flow/model call.

`crates/tj-config/tests/generic_workflows_test.rs` (3 tests) also validates
every shipped graph, exact shared limits, read/write-only tools, the `output.md`
artifact contract and the documented loops. The workflow graph unit tests cover
all three stage/route layouts. See
[checks.md](checks.md) for the exact command environment.

## Limitations

- All three templates are **experimental**; they have scripted integrated
  evidence, not a live supported-model or external user pilot.
- Generic review approval is a human decision with no semantic automatic
  reviewer; it records the exact output identity.
- The research guarantee covers supplied local text files; it does not evaluate
  source truthfulness, only that a citation exists and matches the input.
- No Git, shell, forge, SSH, CI/CD, deployment or external web access is
  available to these profiles.
