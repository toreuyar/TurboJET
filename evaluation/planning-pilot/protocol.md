# Planning pilot protocol (TJ-033)

**Status:** BLOCKED pending real pilot teams\
**Version:** 1 (frozen before any feedback is collected)

## Purpose

Validate whether an approved TurboJET task plan, exported with exact packet and
plan identities, lets downstream engineers start real work with acceptable
clarification, prerequisites, invented scope and correction effort.

This protocol does **not** validate Discovery requirements quality (TJ-027/028)
or Engineering/Delivery execution.

## Participants

- Two pilot teams or two individual engineers with real, non-trivial work.
- At least one participant must be independent of the TurboJET authors.
- Consent must cover notes, exported plan versions and a short interview.
- Exclusion: synthetic personas, agents, scripted model output, internal
  demos without real work, or a participant using a plan the owner edited
  during the session without recording the revision.

If fewer than two real teams complete the pilot, the evidence is exploratory
and TJ-033 remains open.

## Fixed materials

Each pilot receives, from the same approved run:

1. requirements packet JSON and Markdown;
2. structured plan JSON and Markdown;
3. ready-task list showing blocked tasks and reasons;
4. if a task is selected, the handoff payload with criteria and provenance;
5. [handoff-template.md](handoff-template.md);
6. [feedback-template.md](feedback-template.md).

Record the exact packet ID/revision/hash and plan ID/revision/hash in the
handoff. Never regenerate or silently "clean up" the export for a participant.

## Steps

1. **Freeze** the local build, scenario and export identity in
   [freeze-manifest.json](freeze-manifest.json). The current manifest is
   intentionally empty because no external pilot has started.
2. **Recruit and consent** two real teams. Record role/context, not unnecessary
   personal data.
3. **Handoff** the exported plan and identity. Do not explain away gaps; let the
   participant encounter them.
4. **Observe real work** without an agent implementing the task. Record:
   clarifications requested, prerequisites missing, scope the participant
   believed was invented, dependency mistakes, acceptance/verification
   misunderstandings, corrections and time/effort.
5. **Verify scope integrity**: compare every work item the participant started
   with the packet requirement trace or explicit enabling rationale. Any new
   scope must be recorded as a packet-revision request, not silently accepted.
6. **Collect raw feedback** in the participant's words with permission.
7. **Analyze** against the fixed definitions below. Do not change thresholds
   after seeing results.
8. **Owner decision**: release Planning and authorize/defer Engineering (M5),
   or hold/revise. Record reasons and blockers in
   [analysis-template.md](analysis-template.md).

## Operational measures

| Measure | Definition | Evidence |
|---|---|---|
| Requirement loss | Started work cannot be traced to the approved packet and has no enabling rationale, or a confirmed requirement disappears without a recorded decision | Handoff + work-item mapping |
| Clarification load | Number and nature of questions needed before the first work item could start | Interview/notes |
| Prerequisite gaps | Missing access, environment, data, component ownership or sequencing | Notes |
| Invented scope | Work the plan never requested but the participant inferred as necessary; distinguished from legitimate clarification | Notes + requirement map |
| Correction effort | Time/edits needed to make the plan usable, including dependency or acceptance-criterion fixes | Notes + diff of working copy |
| Time to first work item | Elapsed working time from handoff to first real implementation action | Notes |
| Blocked-task honesty | Participant confirms open decisions/blocked tasks were visible and actionable | Export + interview |

## Stop / fail rules

- Any participant starts work outside approved scope and cannot map it to an
  enabling rationale: record a scope-integrity failure and hold.
- The exported packet/plan identity does not match the handoff manifest: the
  session is invalid; do not analyze it as success.
- An agent completes the participant's engineering work: invalid pilot.
- A blocker cannot be explained without inventing a requirement: TJ-033 stays
  blocked pending a packet revision and, if material, a new plan.

## Outputs

- two completed handoff records;
- two raw feedback records;
- one owner analysis with explicit release/defer decision;
- updated task-backlog evidence only after the owner review.

Until then, do not publish a Planning release claim or an M5 authorization.
