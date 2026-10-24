# Structured task-plan contract (Planning)

**Status:** implemented locally (TJ-031/TJ-032); Planning remains
**experimental**, external pilot (TJ-033) is **BLOCKED** and no Planning release
is claimed.

## Accepted input

A plan is always generated from one exact approved requirements-packet revision:

- `packet_id`, `revision`, 64-character canonical SHA-256 `content_hash`;
- the durable packet revision ID and hash stored on the plan row;
- no mutable “latest packet” read is accepted. If a generated plan omits the
  identity, the runtime fills it from the exact revision it supplied. If the
  plan claims a different revision/hash, persistence fails.

The approved packet is immutable during Planning. New product scope is not a
task: it must be recorded in `scope_expansion_requests` and requires a new
packet revision and approval.

## Plan schema v1 (`tj-plan`)

A plan contains `title`, `summary`, optional `source_packet`, `tasks` and
`scope_expansion_requests`. Every task has:

- stable `id`, `title`, `objective`;
- `requirement_refs` to packet requirement/NFR/constraint IDs **or** a non-empty
  `enabling_rationale` for necessary enabling work;
- `dependencies` on other task IDs (missing, self and cyclic references fail);
- `acceptance_criteria`, `components`, `verification`;
- `estimate {size: xs|s|m|l|xl, uncertainty: low|medium|high}`;
- `risks` with severity/mitigation;
- optional owned `decision_refs`.

Structural validation rejects duplicate IDs, empty criteria/components/
verification, invalid estimates, missing/duplicate/cyclic dependencies and
unknown requirement/decision references. An unknown requirement error explicitly
says a requirements-packet revision is required rather than inventing one.

Open owned decisions do not disappear: affected tasks are reported as blocked
with the decision ID/question, and the ready-task contract excludes them.
`scope_expansion_requests` are explicit, never turned into tasks.

## Persistence and approval

- `plan_revisions` is append-only: plan ID, monotonic revision, schema version,
  canonical JSON/hash, source packet revision/hash/ID/number, parent lineage,
  artifact provenance, validation report, change request, creator/time.
- Identical content is a retry-safe no-op. Reviewer edits create a new revision
  with parent lineage; an approved revision is immutable.
- `run_gate_plans` binds the exact plan and source packet to a planning gate in
  the same transaction as gate entry. A new plan revision while the gate is open
  rebinds it and increments the run state version.
- `decide_gate` requires the submitted plan revision/hash and packet revision/
  hash to equal the active binding. A stale edit, missing identity or mismatched
  packet conflicts before any decision/review/transition is written.
- `plan_reviews` records approved/changes-requested with actor/time/cause and
  both exact identities. A rejected plan is retained; a rework creates new
  content and no approval is inherited.

## Readiness and Engineering handoff

`GET /v1/pipeline/runs/{id}/plan/tasks?revision=…&completed=task-a,task-b`
returns each task’s ready flag, dependencies, requirement refs, criteria,
components, verification and blocked reasons. A task is ready only when all
dependencies are in the completed set and no referenced owned decision is open.

`POST /v1/pipeline/runs/{id}/plan/select-task` requires:

- an **approved** exact plan revision ID/hash;
- a task in that revision;
- ready status computed from durable evidence: no incomplete dependency and no
  open blocking owned decision.

The `completed` query on the read-only task list is a review-time preview. The
Engineering selection route **rejects** any `completed_task_ids` supplied by the
caller (`409`) because this local slice has no durable cross-task completion
ledger. Consequently only dependency-free tasks are selectable; a future
multi-task workflow must persist completion evidence before dependent work can
be released. The Manager and publication path independently recompute readiness
from the approved plan/packet instead of trusting the handoff payload boolean.

The durable `plan_task_selections` row and handoff envelope retain the plan ID/
revision/hash, source packet ID/revision/hash, task criteria/dependencies/
verification and provenance. Blocked, stale or unapproved selections conflict.

Planning-gate **approval does not start Engineering**. The run waits with
`gate_state=engineering_task_selection_required`. Only the selection event may
release one Engineering attempt, and the Manager revalidates that exactly one
selection matches the approved plan revision/hash, task ID and ready handoff
payload before spawning any model or repository work. Missing or mismatched
identity leaves the run waiting with an event; no arbitrary task is executed.

## API and export

Authenticated, project-scoped routes:

```
GET  /v1/pipeline/runs/{id}/plan
POST /v1/pipeline/runs/{id}/plan/revisions
GET  /v1/pipeline/runs/{id}/plan/export?format=markdown|json&revision={revision_id}
GET  /v1/pipeline/runs/{id}/plan/tasks?revision={revision_id}&completed={ids}
POST /v1/pipeline/runs/{id}/plan/select-task
```

Markdown and JSON exports come from the same stored revision and include plan/
packet identity, validation report, tasks/dependencies/evidence, blocked tasks,
scope requests, provenance, revision history and reviews. They are readable
without TurboJET.

## Read-only boundary

Planning stages have no shell, repository-edit or write tools. The Manager skips
commit, push and PR creation for the Planning flow even in the full profile.
Repository implementation starts only after that explicit selection, under the
TJ-034 fail-closed shell boundary and TJ-036 durable publication contract. A
failed test/review/acceptance artifact or out-of-scope change cannot produce a
draft PR; unpublished work is retained. Real-forge SSH/PR evidence remains
TJ-038 and is BLOCKED.

## UI

`webui/src/views/Plan.tsx` renders revisions, validation, dependencies,
requirement/evidence links, blocked reasons, scope requests, line diff,
provenance/reviews and Markdown/JSON downloads. It is shown at the planning gate
and as a Plan tab on the run. Task selection is enabled only for a ready task in
the exact approved revision. The view labels Planning **experimental** and states
the read-only boundary.
