# Requirements packet v1 and approval contract

**Status:** local working-tree implementation of TJ-020–TJ-022. Not an external
release claim.

## Schema and provenance

The canonical schema is the typed `RequirementsPacket` document in
`crates/tj-packet/src/lib.rs` (schema version `1`). Its sections are:

- title and problem statement (with source references);
- users, goals and non-goals;
- functional requirements with stable IDs, kind, priority, status, at least one
  acceptance criterion and source references;
- NFRs, constraints, evidence, assumptions, risks;
- contradictions, owned open/decided decisions and unknowns;
- sources (`user_input`, `question_answer`, `stage_artifact`, `owner_decision`,
  `external_document`) with stable references and excerpts.

Generation is performed by the non-interactive `packet` stage in
`workflows/discovery/workflow.smg.json`. Its final JSON is the verdict-tool contract
`{ verdict: "generated", summary, packet }`; the readable packet is mirrored to
the declared stage artifact. The Manager validates the packet, checks every
source against durable run artifacts/answers, and writes an immutable revision:

```
packet_revisions:
  packet_id, revision, content_hash, completeness,
  parent_revision_id, source_artifact_id/hash,
  flow_name, stage_name, change_request, created_by, created_at
```

The hash is SHA-256 over canonical packet JSON. Provenance is never model
supplied: it is the stored stage artifact identity plus the write actor/time.

## Completeness

- `complete`: all requirements confirmed with sources, no open decisions,
  unknowns or contradictions.
- `incomplete`: at least one unknown, open owned decision or non-confirmed
  requirement. This is exportable and approval requires an explicit
  acknowledgement in the API/UI.
- `contradictory`: at least one unresolved contradiction. Export is allowed;
  approval is blocked until a revised packet resolves it.

Missing facts must stay in `unknowns`; confirmed requirements missing a source,
or citing a source that is not a durable input, fail validation.

## Revision and approval binding

- Reviewer revisions are append-only and retain parent lineage, change request,
  actor/time and the new hash. They never inherit an approval.
- A gate entry binds `run_gate_packets` to the latest exact revision/hash in the
  same transaction as `waiting_for_gate`.
- A revision while the gate is open rebinds the review and increments the run
  state version. Approval requests must carry the exact revision ID and content
  hash; old versions/hashes conflict with `409` before any decision or review is
  written.
- The gate decision, packet review, run transition and routing effect share one
  SQLite transaction. Accepted approvals are unique per revision; rejected
  reviews retain the revision, cause, actor/time and lineage.
- Decision idempotency keys are bound to the same packet identity. A retry
  replays the receipt and adds no history; a different packet with the same key
  conflicts.

## Portable export

`GET /v1/pipeline/runs/{id}/packet/export?format=markdown|json&revision=<id>`
requires project access and returns the selected revision (latest by default).
Both formats use the same envelope:

- export schema version;
- packet ID, revision, content hash, completeness and packet content;
- provenance (run, flow/stage, source artifact ID/hash, actor/time);
- full revision history and all gate reviews (approved/changes requested).

Downloads set explicit `Content-Type`, `Content-Disposition` filename and
`Cache-Control: no-store`. Missing revisions, unsupported formats and terminal
rewrites return explicit errors. This export is separate from the stage
conversation transcript export.
