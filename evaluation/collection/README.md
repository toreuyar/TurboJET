# Evaluation collection scaffolding (TJ-028)

**Status:** scaffolding only. No participant data, scores or summaries exist.
External collection is blocked until TJ-026's live evidence, TJ-027's owner
confirmation and real recruitment are available.

## Files

- `manifest.csv` — one row per pair/arm, anonymized IDs only.
- `scores.csv` — one row per reviewer/pair/arm; raw scores, no aggregate
  replacement.
- `interventions.csv` — every human action with a short description.
- `analysis-template.md` — fixed analysis/reporting format.
- `anonymization.md` — removal and retention rules.

## Import rules

1. The coordinator assigns anonymous participant IDs; never commit names,
   emails, employers or customer names.
2. A pair must have two requests and both arms before primary analysis.
   Incomplete pairs remain in the raw table and are labelled exploratory.
3. Never fill missing usage/cost with zero. Use `unknown`.
4. Do not edit a submitted score. Corrections append a new row with
   `supersedes` and a reason.
5. Keep the arm key outside this repository until reviewers submit scores.
6. Publish negative and excluded results alongside positive ones.

## Validation

Run `python3 scripts/evaluation_check.py evaluation/collection` before
analysis. It checks headers, required fields, duplicate IDs, missing arm
labels and accidental obvious email addresses. It cannot prove consent or
anonymity; the coordinator remains responsible.
