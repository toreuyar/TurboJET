# Non-scored calibration example (TJ-027)

**Status:** template. This example is for reviewer calibration only and is
excluded from all results. Do not invent participant data.

## Request

> A team wants a way to export audit events for an external auditor. Events are
> currently visible only in the application. The export must not include
> credentials and must be usable without the product.

## Output A (illustrative, deliberately incomplete)

> Build an audit export. It should include everything important and be secure.
> Add a button. Acceptance: users like it.

## Output B (illustrative, more testable)

> **Problem:** auditors cannot obtain audit events outside the application.
> **Requirement REQ-1 (must):** an authorized administrator can export a
> selected date range as JSON. **Acceptance:** the export contains event time,
> actor, action and object; no credential fields; a reviewer can open it with a
> standard JSON reader. **Unknown (owner: security):** retention window for
> exported files. **Source:** participant request.

## Calibration exercise

Reviewers score A and B on actionability, testability and evidence
traceability, then compare anchors. Expected calibration discussion points:

- A has no observable acceptance criteria and uses vague "secure".
- B has a traceable requirement, explicit acceptance evidence and an honest
  unknown; B still lacks an owner decision on retention.
- Neither output is a real participant result; no scores from this page enter
  the dataset.

## Calibration record

- Reviewer 1 A/B: `__/__`  Reviewer 2 A/B: `__/__`
- Rubric interpretation changes: `____`
- Date: `____`
