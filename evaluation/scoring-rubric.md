# Blinded scoring rubric (TJ-027)

**Status:** fixed before data collection. Do not edit after the first scored
pair; create a new protocol version instead.

## Reviewer procedure

1. Read the participant request and both anonymized outputs (A and B).
2. Score each output independently. Do not infer which arm is the candidate.
3. Score the calibration example first; discuss interpretation; do not include
   calibration scores in results.
4. Submit scores without seeing the other reviewer's scores.
5. Adjudicate disagreements in a note. Preserve both original scores.

## Scores

### Actionability (1–5)

| Score | Anchor |
|---|---|
| 1 | Cannot start work; meaning is unclear. |
| 2 | Major questions block work. |
| 3 | Can start only after several clarifications. |
| 4 | Minor clarifications; work can begin. |
| 5 | A downstream engineer can start and test without asking what a requirement means. |

### Testability (1–5)

| Score | Anchor |
|---|---|
| 1 | No observable acceptance criteria. |
| 2 | Criteria are mostly subjective or unverifiable. |
| 3 | Some criteria are observable; important gaps remain. |
| 4 | Nearly all criteria are observable and falsifiable. |
| 5 | Every criterion is observable, falsifiable and has clear expected evidence. |

### Evidence traceability (1–5)

| Score | Anchor |
|---|---|
| 1 | Claims have no sources or sources are invented. |
| 2 | Many confirmed claims lack a resolvable source. |
| 3 | Mixed; unresolved links remain. |
| 4 | Nearly all confirmed requirements trace to declared sources. |
| 5 | Every confirmed requirement and evidence claim resolves exactly; unknowns are explicit. |

## Counts

For each output record:

- unsupported claims (`count`) — with the claim text;
- omissions (`count`) — participant must-have outcomes/constraints missing or
  mislabeled;
- contradictions (`count`) — plus `critical: yes/no`;
- unknowns that are labeled unknown rather than guessed (`count`);
- owned open decisions (`count`).

## Clarification and intervention

Record reviewer clarification minutes and every human intervention:
`none | edit | re-run | manual rescue | other`. Describe the action and whether
it changed scope.

## Adjudication

If reviewers differ by ≥2 on any 1–5 dimension, write:

- the two scores;
- the evidence each reviewer relied on;
- the reconciled score or `unresolved`;
- whether the disagreement changes the pair-level conclusion.

Do not silently average a disagreement.
