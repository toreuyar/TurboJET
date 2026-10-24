# Discovery beta candidate notes — HOLD

**Status:** HOLD. Not a release. Not approved. Not published.
**Candidate:** working tree after TJ-023–TJ-026 local implementation (no commit).
**Owner decision:** pending. External evidence is BLOCKED.

## What is locally implemented

- First-run/recovery UX: readiness checklist, provider credential action,
  project/profile choice, Chef quick prompts, interrupted/failed guidance,
  keyboard/native controls and narrow layout; bundle measurement is recorded.
- Provider resilience: distinct rate-limit/transient/credential classes, bounded
  per-class transport retries, no tool replay, unknown usage, optional cost/call
  caps, durable redacted activity, resync and support export.
- Recoverable packaging: SQLite-consistent backup/restore, artifacts, encrypted
  secrets with an external key, versioned package layout, readiness, schema
  version, failed-upgrade restore drill and SIGTERM shutdown path.
- TJ-026 real-backend browser suite: eight scripted local-model journeys
  (happy, contradiction, rejection/rework, reconnect/delayed reply, cancel,
  restart/retry, transient/credential provider failure, budget exhaustion).

## Evidence that is still BLOCKED

- Live supported provider/model run with actual availability, version, cost,
  elapsed time and artifact evidence.
- External participants: five participants/two requests each (or explicit
  exploratory shortfall), blinded reviewer calibration and scores.
- Real-forge SSH/PR, remote CI/deployment, release verification/rollback, live
  Zulip account mapping.
- Owner go/no-go and M0–M3 exit review.

## Known limitations

- Local scripted success is software evidence, not live-model or product proof.
- The single-file UI build is ~6.1 MB; a focused code-splitting follow-up is
  required if that impairs the supported environment.
- ADK provider retry is disabled in our stage path so one retry owner exists;
  other provider adapters may retain adapter-level behavior until audited.
- Missing provider usage/cost is stored as unknown, not zero.
- Full SDLC, sandbox, external publication idempotency and exact conversation
  resume remain unproven/conditional.

## Release gate

Negative evaluation or missing live evidence produces **HOLD or revision**, never
an automatic beta release. See the task backlog records TJ-026/TJ-028/TJ-029
and the evaluation protocol. Do not edit this document into a release claim
without adding real evidence to the ledger.
