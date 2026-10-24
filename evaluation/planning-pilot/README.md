# TJ-033 Planning pilot materials

**Status:** prepared · **external pilot validation BLOCKED**\
**Local scripted evidence:** [planning-pilot scenario](../../tests/qa/scenarios/planning-pilot.md)

This directory contains the handoff and feedback materials for taking an
approved, exported TurboJET plan to two real engineering teams/engineers. It
contains **no participants, feedback, scores, quotes or pilot result**. A local
scripted browser journey is not a pilot. Do not mark TJ-033 complete, authorise
Engineering (M5) or claim Planning release until the owner evidence below is
real.

## Artifacts

| File | Purpose |
|---|---|
| [protocol.md](protocol.md) | Fixed pilot steps, inclusion/exclusion, evidence and stop rules |
| [handoff-template.md](handoff-template.md) | Per-team packet/plan export manifest and prerequisite record |
| [feedback-template.md](feedback-template.md) | Raw clarification/scope/correction record, no invented answers |
| [freeze-manifest.json](freeze-manifest.json) | Intentionally unfilled build/scenario identity for the eventual pilot |
| [analysis-template.md](analysis-template.md) | Owner summary and release/defer decision, filled only from real evidence |

## Required plan identity

Every pilot handoff must name and ship the same immutable identities:

- requirements packet `packet_id`, revision and 64-character content hash;
- structured plan `plan_id`, revision and content hash;
- source packet revision/hash recorded in the plan;
- selected task ID, acceptance criteria, dependencies and provenance when a
  task is handed to Engineering.

Exports come from the authenticated routes:

```
GET /v1/pipeline/runs/{run}/packet/export?format=json|markdown&revision={revision_id}
GET /v1/pipeline/runs/{run}/plan/export?format=json|markdown&revision={revision_id}
GET /v1/pipeline/runs/{run}/plan/tasks?revision={revision_id}&completed={task_ids}
POST /v1/pipeline/runs/{run}/plan/select-task
```

## Hard boundaries

- Planning is read-only: no repository write, commit, push or pull request.
- Engineering task selection is refused for blocked tasks and for an
  unapproved/stale plan revision.
- New scope belongs in `scope_expansion_requests` and needs a new approved
  packet revision; it must not be invented as a task.
- External pilot feedback must be recorded verbatim/paraphrased with consent.
  Agents, scripted fixtures and synthetic personas may not supply feedback.
- A negative result produces revision/hold, not automatic release.

## Current blocker statement

> External pilot validation is **BLOCKED**. No real team has used an exported
> plan for real work. Local scripted tests pass; they are contract evidence, not
> user evidence. The owner has not accepted a Planning release or authorised
> Engineering.
