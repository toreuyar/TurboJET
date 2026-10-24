# Discovery live journey scenario (TJ-026) — BLOCKED

**Status:** BLOCKED. Not executed. This file is a prepared scenario, not
evidence. Do not mark passed until real operator evidence is attached.

## Purpose

Exercise the complete repository-free Discovery journey through the browser
against a **real backend** and a **supported provider/model**, then export the
approved packet. This is separate from the local scripted suite
(`webui/tests/e2e/backend-journey.spec.ts`), which is software evidence only.

## Prerequisites (all required)

- A disposable `TURBOJET_HOME`, database and workspace; never production state.
- A supported provider/model configured in the chosen config; record the
  provider id, model id/version, base URL and credential env **name** (never the
  value). The config must not contain the legacy `chat`/Zulip block: startup
  rejects it. The default `config/turbojet.json` is chat-free and loads the
  shipped `workflows/` packages; copy it to a disposable path and confirm the
  Discovery package source (`workflows/discovery`) is present before starting.
- `TURBOJET_MASTER_KEY` for encrypted secrets.
- Built UI and the real `tj-api` binary on a disposable port.
- Explicit operator authorization and a cost ceiling. No calls are made by the
  preparation itself.

## Steps

1. Start the backend on a disposable host/port. Confirm `/api/ready` reports
   `ready` and record the returned version.
2. Open the UI; create the first administrator. Confirm the readiness checklist
   shows the real provider/model and no missing credential action.
3. Create a repository-free Discovery project; confirm the supported profile
   badge and no Git/forge/SSH/shell activity.
4. Start a run. Answer at least one clarifying question, then let the packet
   generate.
5. At the gate, review evidence/unknowns/decisions, download the exact packet
   JSON/Markdown, and approve the exact revision/hash.
6. Confirm the run ends Discovery (no Planning/Engineering/Delivery flow),
   export support/diagnostics, and verify credentials are redacted.
7. Repeat negative cases separately: contradictory packet approval blocked;
   rejection/rework; reconnect/delayed answer; cancel; restart/retry; provider
   failure; budget exhaustion. Use a disposable reset between cases.
8. Record evidence in the table below and in the operation ledger.

## Evidence to record

| Field | Value |
|---|---|
| Date/operator | |
| TurboJET version / git revision | |
| Provider id / model / version | |
| Cost cap / actual cost (or `unknown`) | |
| Elapsed time / token usage (or `unknown`) | |
| Packet revision + content hash | |
| Artifact/export paths | |
| Negative-case outcomes | |
| Skipped/blocked cases and reason | |
| Logs (credential-checked) | |

## Pass criteria

- Every step uses the real backend and a real supported model; no mocked API.
- Exact packet revision/hash is what was approved; export matches.
- No manual database repair; no credential appears in activity/support export.
- All negative cases reach their documented outcome or are explicitly BLOCKED.

## Current status

**BLOCKED:** no live supported-provider run has been performed. Local scripted
success does not satisfy this scenario, and TJ-027/TJ-028/TJ-029 remain pending
external evidence.
