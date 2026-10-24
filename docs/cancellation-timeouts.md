# Cancellation and timeout contracts

Working-tree implementation, 2026-10-10. Tasks: TJ-008 and TJ-009 (verified).

## Cancellation and cleanup

The manager retains each flow task and its process supervisor. Cancellation:

1. Persists the terminal state and fences descendants through the TJ-007 lifecycle transaction.
2. Aborts and awaits the flow future, which drops in-flight model, question, and tool awaits.
3. Awaits registered shell-process supervisors before touching the workspace.
4. Cleans the workspace and returns the command acknowledgment.

The stop/join phase has a five-second bound. A join, reaping, or process-group
shutdown failure records `execution_shutdown_failed` and retains the workspace.
Deletion also requires a joined acknowledgment and refuses workspaces with a
recorded unconfirmed shutdown, including after restart. A persistence failure
still stops active execution, but returns an error and preserves the workspace.
Repeated cancellation does not rewrite terminal history or repeat cancellation
events. REST returns authoritative run state; Chief receives failure rather than
claiming that an unsuccessful command cancelled the job.

Each shell command runs in its own Unix process group. Cancellation, shell
timeout, and normal shell exit terminate ordinary descendants, reap the direct
child, and check for remaining live group members. Linux checks ignore zombies
(which cannot write) and processes disappearing during `/proc` enumeration.
Process cleanup itself is bounded at four seconds; failure propagates to the
manager. Background child processes are not allowed to outlive a successful
tool call.

This is basic process-tree cancellation. A process that deliberately escapes
with a new session/group requires the sandbox work in TJ-034. The five-second
bound is for stopping an active execution, not for time queued behind an existing
manager operation or external chat/worktree-cleanup latency.

SIGTERM and SIGINT invoke the same joined cancellation path for active flows.
New-run admission closes before shutdown is enqueued. Waiting gates have no live
flow task and retain their durable state. HTTP cancellation/deletion now await
manager acknowledgments; they no longer report success immediately after enqueue.

## Timeout policy

- **Model calls:** existing interactive runner ADC timeout applies per model
  call, not to the user's answer wait. Non-interactive stages retain their
  whole-loop timeout. Existing runner timeout regressions cover these semantics.
- **Attempt watchdog:** retains the existing `SMG.max_steps × 600 seconds`
  wall-clock backstop. It includes human question waits within an executing flow.
  Gate review suspends this attempt timer. Every start, rework, and resume arms a
  new generation; a queued tick from an earlier generation cannot stop it.
- **Run budgets (TJ-016):** question/model/tool/correction/nudge counters and
  active-time intervals live in SQLite. Active time starts before model/tool work
  and is closed on completion, cancellation, timeout or restart; restart uses the
  run's last durable activity so downtime is not charged. A denied model/tool or
  correction charge fails the stage/flow visibly; it is not retried. Exact
  provider token/cost accounting remains TJ-024.
- **Restart (TJ-017):** a process stop marks in-flight runs `interrupted`, joins
  no old worker (the process is gone), fences attempts and closes active-time at
  the last durable activity. No model/tool/publication replay occurs. An explicit
  retry starts a new attempt. Gate routing has a durable pending effect and may be
  reconciled once; an uncertain publication effect requires acknowledgment.
- **Absolute run deadline:** optional `run_timeout_secs` in `turbojet.json`.
  When configured, it must be positive and representable. New runs persist an
  immutable `deadline_at`. It includes model/tool work, questions, and gates,
  survives restart, and cannot be extended by rework/resume or later config edits.
  Omission leaves existing deployments without a total run deadline; it does not
  disable their attempt watchdogs. Existing runs migrate with no absolute deadline.
- The earlier of the applicable attempt timeout and absolute deadline is armed.
  Expired deadlines also fence new flow/stage writes and completion claims in the
  database, so a delayed timer cannot allow successful late routing.
- Timeout uses the same joined stop-before-cleanup path as cancellation. Durable
  `run_timed_out` events distinguish attempt watchdog from absolute deadline.
- A returned `FlowOutcome::WaitingForInput` is now a visible failure. Live user
  questions remain inside the stage's running future; a worker that has already
  returned cannot consume a later answer and must not advance as success.

## Executed verification

TJ-008:

- `CARGO_HOME="$HOME/.cargo" cargo test --workspace --locked --offline`: 286 passed.
- Final `cargo test -p tj-tools -p tj-manager -p tj-api --locked --offline` with
  the same `CARGO_HOME`: 102 passed, no compiler warnings.
- Seven added regressions cover execution-drop acknowledgment, cancellation of
  real shell descendants, shell timeout, normal-exit background cleanup,
  idempotent/late cancellation, graceful shutdown, and injected join failure
  preserving the workspace. These use local processes, not live providers/forges.
- The final check fixes an observed process-exit race: Linux can return `ESRCH`
  as well as `ENOENT` when a `/proc/<pid>/stat` entry disappears during inspection.

TJ-009: `CARGO_HOME="$HOME/.cargo" cargo test --workspace --locked --offline`
passes **293 tests**, with no compiler warnings. Seven new regressions cover
deadline immutability, elapsed-deadline write/completion fencing, old watchdog
generations, question-return/timeout races, a real timer at an expired gate
deadline, returned-wait failure, and automatic watchdog arming on resume.

Temporary logs are
`/tmp/opencode/turbojet-cancellation-tests.log`,
`/tmp/opencode/turbojet-cancellation-final.log`, and
`/tmp/opencode/turbojet-timeout-tests.log`.
