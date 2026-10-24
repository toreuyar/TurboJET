# Delivery staging/verification/rollback live drill (TJ-039/TJ-040) — BLOCKED

**Status:** NOT RUN — no real external CI/CD, deployment target or health probe\
**Tasks:** TJ-039, TJ-040\
**Local stand-in:** `crates/tj-api/tests/delivery_harness.rs`,
`crates/tj-delivery`, `crates/tj-db/src/delivery.rs`, and
[`docs/delivery-integration.md`](../../../docs/delivery-integration.md)

> This is the operator procedure for real evidence. The disposable local HTTP
> adapter and deterministic callbacks are not deployment proof. Do not check
> TJ-039/TJ-040, recommend release, or claim a verified rollout until every
> prerequisite and evidence item below is real.

## Required prerequisites (none are satisfied here)

- [ ] Owner-approved target system and environment names (staging and
      production), with the external CI/CD retaining execution authority.
- [ ] A real Forgejo instance with Actions enabled, a disposable repository,
      `delivery.yml` workflow, runner capacity and non-production deployment
      targets.
- [ ] A least-privilege project token and encrypted callback secret supplied
      through environment variables; a real `callback_base_url` reachable by the
      workflow.
- [ ] A published Engineering draft PR whose remote SHA matches the exact
      publication commit; a human owns merge.
- [ ] Staging and production health/acceptance probes owned by an identity
      distinct from the deployment dispatcher.
- [ ] A disposable database/workspace and evidence directory under
      `/tmp/opencode/` (or an access-controlled store).
- [ ] A named release operator and reviewer. Production action requires both an
      exact-revision authorization and the owner's release decision.

## Execution

1. Configure the adapter from `docs/delivery-integration.md`; restart and confirm
   the Delivery panel reports the real forge/workflow. Confirm missing
   token/secret fails startup closed.
2. Request a **staging** release for the exact publication. Capture the request
   operation key, release ID and external run URL.
3. Let external CI run the exact commit. Confirm signed CI, deployment,
   independent verification and observation callbacks are accepted with exact
   revision and monotonically increasing sequence.
4. Verify `recommendation=ready` only after all evidence. Replay one callback and
   confirm `replayed=true` with no state change. Send a lower sequence and
   confirm a persisted rejected callback plus unchanged state.
5. Force a CI test failure on a fresh staging operation. Confirm recommendation
   `blocked` and that no production authorization can be created.
6. Force a failed or unknown health verification. Confirm the deployment is not
   reported successful; if unknown, reconcile by signed callback/poll.
7. Create the production authorization decision with the exact publication,
   staging release, verification/observation evidence and a written reason.
   Confirm a production request without that decision is refused.
8. Dispatch production. Independently confirm the actual deployed revision and
   health evidence before accepting `recommendation=ready`.
9. Restart the TurboJET process between dispatch and callbacks. Confirm in-flight
   state becomes `unknown`, never success. Resolve with a real signed callback or
   explicit poll. Exercise explicit re-dispatch only after acknowledging that the
   old attempt may have landed; confirm workflow operation-key deduplication.
10. Authorize and run a rollback to a different known-good revision. Independently
    verify the deployed revision equals the target and health passes. Send a
    later unknown report and confirm it cannot become success.
11. Capture external CI run URLs/logs, deployed revision, health evidence, callback
    audit, decisions and the final delivery bundle export.

## Acceptance evidence to attach

- Exact run/publication/task/plan/packet/PR/commit identities and operation keys.
- Staging and production external workflow run URLs, runner identity, timestamps
  and deployed revision evidence.
- Independent health/acceptance evidence hashes, verifier identity and results.
- Duplicate/stale callback audit rows, rejected decision errors and the final
  recommendation with reasons.
- Failure drill evidence: failed CI/verification blocks release; unknown never
  succeeds; restart reconciliation is explicit.
- Rollback authorization, target revision, verified deployed revision and health.
- Operator/reviewer sign-off and explicit limitations.

## Blocked result (current)

The local drills pass against a disposable HTTP adapter and SQLite only. No real
Forgejo Actions run, deployment target, health evidence, production authorization
or rollback has occurred. TJ-039/TJ-040 remain externally BLOCKED.
