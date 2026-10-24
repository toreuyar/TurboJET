# Lifecycle persistence contract (TJ-007)

Implemented and verified in the working tree on 2026-10-10 (TJ-007).

- Every loaded run, flow, and stage carries a private database revision. Saving
  compares that revision inside a write transaction; a stale writer conflicts,
  including when another writer kept the same status. New records cannot replace
  existing IDs. Deleted records cannot be recreated by a previously loaded writer.
- Run transitions: running → waiting_for_gate/interrupted/completed/failed/cancelled;
  waiting_for_gate → running/interrupted/completed/failed/cancelled;
  interrupted → running/completed/failed/cancelled.
- Flow transitions: pending → running/failed/timed_out;
  running → completed/failed/timed_out.
- Stage transitions: pending → running/failed/timed_out;
  running ↔ waiting_for_input; either active state → completed/failed/timed_out.
- Same-state metadata updates require a current revision. Terminal records are
  immutable. Retrying or resuming execution creates a new attempt identity.
- Lifecycle state writes and their lifecycle audit events share a transaction.
  A failed event insert rolls back the state write. Conflicts are errors, never
  successful no-ops that permit a caller to advance.
- Flow completion delivery is accepted at most once for the active execution
  attempt. Acceptance must persist before routing or external publication.
- Cancellation shutdown/join ordering is TJ-008; deadline/watchdog policy is
  TJ-009. Atomic persistence alone does not stop an already-running tool.

## Attempt identity and migration

`pipeline_runs`, `flows`, and `stages` gain a `revision` column, defaulting to zero
for existing rows. New in-memory records carry an insert-only sentinel; loaded
records carry their database revision. These internal values are not accepted
from API JSON. Saves use `BEGIN IMMEDIATE`, a revision-conditional write, and an
upsert that preserves row insertion order (unlike the former replacement).

The manager reserves a unique flow ID and an `execution_attempts` row before
spawning the flow. `current_flow_id` fences earlier flow/stage writers; stage IDs
remain unique per stage execution. An active unconsumed attempt cannot be
replaced by a duplicate start. Every `FlowDone`, including panic results, carries
the attempt ID. The manager persists a single-use claim before handling it.
Stale/duplicate deliveries produce no routing, publication, or domain events.

Startup fences previous-process attempts and does **not** schedule an automatic
replacement. A run left `running` becomes `interrupted`; accepted answers,
artifacts and open questions are preserved. `run_recovery` records the durable
resume flow/stage and whether external effects are unknown. Only an explicit
`retry` transition starts a replacement attempt and fences/finalizes old active
children. This retains the existing single-instance deployment assumption; it is
not a multi-manager lease protocol. At-most-once event acceptance is not
exactly-once remote publication.

Pending follow-on work is recorded in `pending_effects`. A gate decision writes
its `gate_route` effect in the same transaction as the decision; reconciliation
claims it only when no execution attempt exists after it, so a crash between
acceptance and routing routes exactly once. Accepted flow completions write a
`flow_completion` effect; if publication/routing cannot be proven after a crash
the effect becomes `ambiguous` and is never replayed automatically.

Run budgets are separate durable counters. Question rounds, model calls, tool
calls, output corrections, silent-turn nudges and active time persist across
attempts/rework/retry. Charges are transactional and idempotent by operation
key; every call in a multi-call response is charged. A budget denial fails the
flow instead of spending a retry.

Run termination atomically finalizes active descendants and records their
lifecycle audit events. Flow/stage saves also require an active parent and the
current flow identity. Failed state persistence stops completion routing and
terminal/gate notifications. Stage executors reload after awaited runner turns
because question handling legitimately updates the row during the wait; they
only rebase on active rows and then save with the newly loaded revision.

## Verification

- `CARGO_HOME="$HOME/.cargo" cargo test --workspace --locked --offline`: **279
  passed**, zero failures/ignored tests/compiler warnings.
- After the final gate-persistence failure handling adjustment,
  `CARGO_HOME="$HOME/.cargo" cargo test -p tj-db -p tj-manager --locked --offline`:
  **40 passed**, zero failures/compiler warnings.
- Twelve new regressions: ten in `tj-db::lifecycle::tests`, two in
  `tj-manager::actor::lifecycle_tests`. Includes a barrier-controlled race on
  separate SQLite connections (one success, one explicit lifecycle conflict),
  same-status stale writers, terminal descendant fencing, deleted-row resurrection,
  audit failure rollback, attempt reservation rollback, restart identity,
  stable row ordering, and manager-level duplicate/old-attempt rejection.
- Existing question-channel and flow-resume tests pass. The stale-stage fixture
  now creates children while the run is active, then moves to the gate, matching
  the real lifecycle rather than inserting new work into a paused run.

Local logs: `/tmp/opencode/turbojet-lifecycle-tests.log` and
`/tmp/opencode/turbojet-lifecycle-final.log`. No live provider/forge calls or
production database migration was performed. Changes are not committed/deployed.
