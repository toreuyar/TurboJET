# Discovery beta evaluation protocol (TJ-027)

**Status:** prepared, not executed. No participants, scores or results exist.
**Owner:** product/research. **Gate:** D7 was accepted in project plan §3.1
(local record);
scoring must not start until the owner reconfirms the frozen candidate.

This protocol evaluates the Discovery beta candidate produced by TurboJET
against a paired baseline. It is a product-research instrument, not release
proof. Local scripted journeys (TJ-026) do **not** substitute for these
participants.

## 1. Questions

1. Does the candidate produce a requirements packet that a real downstream
   engineer can act on with less clarification than the paired baseline?
2. Are unsupported claims, omissions, contradictions and untestable criteria
   fewer than in the baseline?
3. What time, cost, reviewer effort and human intervention are required?
4. Does the owner see enough evidence to continue, revise or stop?

## 2. Design

- **Target:** five participants, two requests each (ten paired observations).
  If recruitment falls short, results are labelled exploratory, never
  presented as the target sample.
- **Pairing:** each participant supplies two comparable requests. Requests are
  counterbalanced: one to the candidate, one to the baseline. If both requests
  are used on one arm, record that deviation and exclude the pair from the
  primary comparison.
- **Baseline:** the same model and the same request with a neutral
  requirements-writing prompt. Prompt text is frozen in the manifest.
- **Blinding:** reviewers receive anonymized outputs labelled only A/B. Arm
  labels are held by the coordinator until both reviewers submit scores.
- **Reviewers:** two independent reviewers. They first score one shared,
  non-scored calibration example and reconcile the rubric interpretation.
  Calibration scores are not included in results.
- **Freeze:** model identifier/version, provider, candidate Git revision,
  prompts, schemas, tool policy and this protocol are recorded before the
  first request. Any change creates a new candidate and invalidates the run.
- **No threshold shopping:** thresholds in §5 are fixed before data collection.

## 3. Recruitment and consent

- Recruit practicing product/engineering participants who routinely write
  requirements. Do not recruit project contributors or agents.
- Use [consent.md](consent.md). Consent covers recording, anonymization,
  retention, withdrawal and publication of anonymized scores.
- Participants may withdraw before anonymization; after anonymization,
  withdrawal is limited to the identifiable raw record.
- Do not record names, employer names, customer names or proprietary request
  content in published artifacts. Raw identifiable records stay in the
  coordinator's access-controlled store.

## 4. Procedure

1. Coordinator freezes and records the manifest.
2. Participant signs consent and supplies two requests using
   [paired-request-template.md](paired-request-template.md).
3. Candidate run uses the supported Discovery profile. Baseline run uses the
   frozen baseline prompt. Both use the same model/version and limits.
4. Record time-to-first-review, model/tool calls, cost (or `unknown`), token
   usage (or `unknown`), reviewer minutes and every human intervention.
5. Export the candidate packet with revision/hash and the baseline output.
   Normalize formatting as described in [scoring-rubric.md](scoring-rubric.md).
6. Reviewers score independently and then reconcile disagreements in a short
   adjudication note. Do not average away a disagreement without explanation.
7. Coordinator unblinds, joins the record, and runs the analysis template.
8. Publish anonymized raw scores, summaries, negative results and limitations.

## 5. Fixed thresholds

The owner accepts a **continue** recommendation only if all are true:

- mean packet actionability ≥ 3.5/5 across reviewers and pairs;
- mean unsupported-claim count is strictly lower than the paired baseline;
- no pair has a critical contradiction that the candidate presents as
  resolved without an owned decision;
- median reviewer clarification time is lower than baseline by at least 20%;
- no severe consent, credential or data-loss incident.

Otherwise the outcome is **revise** or **stop**. These thresholds may not be
changed after results are known. A change requires a new protocol version and
is reported alongside any earlier result.

## 6. Operational definitions

| Dimension | Definition | Scale / measure |
|---|---|---|
| Unsupported claim | A statement presented as fact without a source in the packet or participant input. | count |
| Omission | A participant-stated must-have outcome, constraint or decision that is absent or mislabeled. | count |
| Contradiction | Two packet statements/inputs that cannot both be true. | count; critical if it changes scope or policy |
| Testability | Each acceptance criterion is observable and falsifiable by a reviewer. | 1–5 |
| Actionability | A downstream engineer could start work without asking what the requirement means. | 1–5 |
| Evidence traceability | Confirmed requirements resolve to declared sources. | 1–5 |
| Clarification effort | Reviewer minutes spent asking the participant what the output means. | minutes |
| Intervention | Human correction, re-run, manual edit or rescue. | count + description |
| Cost | Provider-reported monetary cost; if absent, `unknown` (never zero). | micro-units or unknown |

## 7. Formatting normalization

Before scoring, remove product branding, candidate/baseline labels, prompt
boilerplate, timestamps, internal IDs and formatting-only differences. Do not
add headings, rewrite requirements, fix contradictions or fill omissions.
Record every normalization action in the pair record.

## 8. Analysis and publication

Use [collection/analysis-template.md](collection/analysis-template.md). Report
each pair, both reviewer scores, means, ranges, disagreements, negative
results, missing usage/cost and deviations. Do not publish aggregate scores
without the underlying anonymized rows. The owner records continue/revise/stop
and the next scope in the evidence ledger; release remains a separate decision
(TJ-029).

## 9. Exclusions

- No synthetic participants, simulated answers or invented scores.
- No live provider claim unless every run records model/version, config hash,
  cost and elapsed time.
- No claim that this protocol validates Planning, Engineering, Delivery or a
  sandbox.
