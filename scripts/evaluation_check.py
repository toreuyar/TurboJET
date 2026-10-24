#!/usr/bin/env python3
"""Validate external evaluation collection scaffolding (TJ-028).

Checks structure only. It does not prove consent or anonymity.
"""
import csv
import re
import sys
from pathlib import Path

EXPECTED = {
    "manifest.csv": ["pair_id", "participant_id", "request_slot", "arm", "candidate_revision", "model_id", "model_version", "execution_bundle_hash", "baseline_prompt_hash", "elapsed_ms", "model_calls", "tool_calls", "input_tokens", "output_tokens", "cost_micros", "packet_revision", "packet_content_hash", "review_status", "notes"],
    "scores.csv": ["reviewer_id", "pair_id", "arm", "actionability", "testability", "evidence_traceability", "unsupported_claims", "omissions", "contradictions", "critical_contradictions", "labeled_unknowns", "open_decisions", "clarification_minutes", "interventions", "calibration", "supersedes", "submitted_at", "notes"],
    "interventions.csv": ["pair_id", "arm", "action_type", "description", "changed_scope", "reviewer_id", "duration_minutes", "recorded_at"],
}
EMAIL = re.compile(r"[\w.+-]+@[\w-]+\.[\w.]+")
ARMS = {"candidate", "baseline"}


def validate(root: Path) -> list[str]:
    errors = []
    for name, header in EXPECTED.items():
        path = root / name
        if not path.is_file():
            errors.append(f"{name}: missing")
            continue
        with path.open(newline="", encoding="utf-8") as handle:
            reader = csv.reader(handle)
            rows = list(reader)
        if not rows:
            errors.append(f"{name}: empty")
            continue
        if rows[0] != header:
            errors.append(f"{name}: header mismatch\nexpected: {','.join(header)}\nfound:    {','.join(rows[0])}")
            continue
        for index, row in enumerate(rows[1:], start=2):
            if len(row) != len(header):
                errors.append(f"{name}:{index}: expected {len(header)} columns, found {len(row)}")
                continue
            joined = ",".join(row)
            if EMAIL.search(joined):
                errors.append(f"{name}:{index}: obvious email address; anonymize before analysis")
        if name in ("manifest.csv", "scores.csv"):
            seen = set()
            for index, row in enumerate(rows[1:], start=2):
                if len(row) != len(header):
                    continue
                record = dict(zip(header, row))
                if record.get("arm") and record["arm"] not in ARMS:
                    errors.append(f"{name}:{index}: arm must be candidate or baseline")
                if not record.get("pair_id"):
                    errors.append(f"{name}:{index}: pair_id is required")
                key = (record.get("pair_id"), record.get("arm"), record.get("reviewer_id", ""), record.get("request_slot", ""))
                if key in seen:
                    errors.append(f"{name}:{index}: duplicate key {key}")
                seen.add(key)
    return errors


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("evaluation/collection")
    errors = validate(root)
    if errors:
        print("FAIL")
        for error in errors:
            print(f"- {error}")
        return 1
    print("OK: evaluation collection structure is valid (consent/anonymity not verified)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
