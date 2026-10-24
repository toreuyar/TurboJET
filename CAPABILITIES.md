# TurboJET Capabilities and Evidence

> **2026-10-25 implementation delta (Batch 4 final):** the current working tree
> runs arbitrary registry catalog workflows with explicit `catalog` and
> capability declarations, supports an empty registry, freezes capability
> contract v2 into each bundle, and keeps v1 behind one isolated compatibility
> adapter. Shipped workflows are self-contained packages under `workflows/`
> (graph, agents, prompts, schemas); there is no shipped shared agent/schema
> tree, and `adc_dir` is legacy-only for old file/directory graphs. The
> canonical persona is **Chief** (legacy `chef` id/env/browser tokens are bounded
> migration only). Current verification: **508 default Rust tests**, **42 web
> unit tests**, production build, **8 mocked browser tests**, **9 scripted
> real-backend journeys** (including a plan-less publication journey with
> durable shell test evidence and a local draft PR), asset-free **508 Rust + 42
> web** in a disposable copy, optional Rust **211** + local SDLC **2** + web
> examples **11**, legacy v1 **2**, and **9** offline
> backup/restore/package tests. Packaging includes the four `workflows/`
> packages and excludes `config/local/**` and secret/runtime paths regardless of
> Git ignore state; empty-start, shipped-package and synthetic-source smokes all
> pass. These are uncommitted working-tree results, not a deployed release; live
> provider/forge/deployment/external evidence is not claimed. See
> [checks](docs/checks.md),
> [access](docs/project-access.md), [lifecycle](docs/lifecycle-contract.md), and
> [cancellation/timeouts](docs/cancellation-timeouts.md). The July snapshot
> below is historical.

This document records evidence maturity, not product aspiration. Current internals are in [ARCHITECTURE.md](ARCHITECTURE.md); intended direction is in [VISION.md](VISION.md). The review basis is committed source at `master@2cf96d4` plus explicitly identified uncommitted documentation and evidence; this document is not necessarily part of that committed snapshot.

## Snapshot and Disclaimer

- **Snapshot:** committed source at `master@2cf96d4` plus explicitly identified uncommitted documentation and evidence examined by the assessment; these documents are not necessarily part of that commit.
- **Evidence review date:** 2026-07-20.
- **Method:** static source, configuration, manifest, history, document, scenario, and project-memory review.
- **No-rerun disclaimer:** no build, unit test, Clippy, server startup, QA scenario, external integration, security audit, or dependency audit was run on 2026-07-20. Historical test statements below are provenance records, not tests run today.

## Evidence Vocabulary

The columns are independent. A `Yes` in one column does not imply any other column.

| Term | Meaning |
|---|---|
| **Documented** | A current or historical requirement, design, plan, scenario, or memory record describes the capability. |
| **Implemented** | The snapshot contains a source/configuration path implementing all or part of it. |
| **Static-tested** | Test source directly exercises the named capability. This does not establish that tests pass now. Historical aggregate results belong in the Historical Evidence Ledger and do not upgrade direct coverage. |
| **Operationally-proven** | A dated record shows an end-to-end execution. Scripted/cooperative proof is qualified and is weaker than independent use. |
| **Externally-validated** | Independent target users or production evidence demonstrate useful outcomes, not merely that the software ran. |

Status values are `Yes`, `Partial`, and `No`. `Partial` means narrow, stale, incomplete, qualified, or mixed evidence.

Scenario existence is only **Documented** evidence. A scenario becomes **Operationally-proven** evidence only when a dated execution record identifies the snapshot/configuration, result, and provenance. A historical execution record is not a test run on 2026-07-20.

## Capability Matrix

| Capability | Documented | Implemented | Static-tested | Operationally-proven | Externally-validated | Qualification |
|---|---:|---:|---:|---:|---:|---|
| API and Concierge | Yes | Yes | Yes | Partial | No | OpenAI-compatible chat, run control, SSE, and Concierge exist. Project-scoped bearer sessions and instance-admin gates are implemented and regression-tested; live external multi-user operation is not proven. |
| Discovery through Zulip | Yes | Yes | Yes | Partial | No | Historical scripted/cooperative 9/9 lacks snapshot/configuration, raw artifacts, and operator metadata required by the new ledger; it is not external validation. |
| SMG routing and gates | Yes | Yes | Yes | Partial | No | Graph/config validation runs at admission and runtime; routing is verdict-first and fails closed. Gate decisions are durable/idempotent with pending routing effects. Discovery gates bind the exact requirements packet revision/hash; integrated local harnesses exercise the real chain, while live external operation is not proven. |
| Repository-free Discovery beta | Yes | Yes | Yes | Partial | No | Supported profile ends at packet approval, grants no shell/edit/forge access, generates an evidence-linked packet, persists immutable revisions, and exports the approved revision as portable Markdown/JSON. External evaluation remains unrun. |
| ADK and provider execution | Yes | Yes | Yes | Partial | No | In-process ADK and model tests exist; provider breadth and long-running behavior are not proven. |
| Native tools | Yes | Yes | Yes | Partial | No | Read/write/edit/bash and path tests exist; model shell commands fail closed under a Landlock/seccomp boundary, but arbitrary Engineering success and hostile-code safety are not proven. |
| ADC contracts and artifacts | Yes | Yes | Yes | Partial | No | ADCs are frozen into the execution bundle; missing/invalid ADC, prompt, schema, provider or model fails admission. Interactive and non-interactive completion validate structured output and required artifacts locally. The schema vocabulary is deliberately limited, not full JSON Schema. |
| Full Discovery -> Planning -> Engineering -> Delivery | Yes | Partial | Partial | Partial | No | Discovery→Planning and one-task Engineering are locally exercised through the real API/Manager/Flow/Stage chain plus local forge scenarios. External CI/CD delivery contracts are locally drilled, but real deployment/rollback, a real-forge pilot and external Planning evidence are not proven, so the full lifecycle is not a completed capability. |
| Task worktrees and branch lifecycle | Yes | Yes | Partial | Partial | No | Direct tests cover parts of worktree handling; historical sprint aggregates do not establish direct coverage of the complete lifecycle, and a worktree is not security isolation. |
| Forgejo draft PR lifecycle | Yes | Yes | Yes | Partial | No | Durable publication identity, push/PR reconciliation, lost-response/no-duplicate and local HTTP Forgejo-protocol scenarios pass. No real Forgejo remote, SSH push or human reviewer evidence exists. |
| AES-GCM secrets storage at rest | Yes | Yes | Yes | No | No | AES-256-GCM storage and DB wiring exist. ManagerHandle/SystemOperations removes a direct LLM API path, not an LLM security boundary: Bash inherits process environment and host filesystem access. Secret/SSH routes require an instance administrator and diagnostic logging is credential-safe by regression test; `tj-ops` now provides external-key backup/restore and versioned packaging, while rotation and complete audit controls remain incomplete. |
| SSH key generation and injection | Yes | Yes | Partial | No | No | Atomic 0600 project key + verified known_hosts, strict/batch SSH and finite Git timeouts exist; real Forgejo SSH clone/push remains BLOCKED. |
| Cancellation and timeout recovery | Yes | Yes | Yes | Partial | No | Cancellation/timeout joins the flow future and shell process supervisors before cleanup; late events/answers are fenced and unpublished work is retained. The boundary blocks ordinary escape syscalls but is not a hostile-code container. |
| Restart recovery | Yes | Yes | Yes | Partial | No | Startup marks in-flight runs interrupted, preserves accepted answers/artifacts/open questions, records durable recovery state and reconciles pending gate effects once. Retry is explicit and starts a new attempt. Exact model-conversation resume and idempotent external publication are not implemented. |
| Logging and observability | Yes | Partial | Partial | Partial | No | Trace/timing/JSONL and credential-safe diagnostic tests exist; some legacy log reader routes remain compatibility shells and no SLO/alerts are evidenced. |
| Deployment and operations | Partial | Partial | No | No | No | Scripts and a stale Dockerfile exist; no supported release/deployment/rollback/monitoring system is proven. |
| External product validation | Yes | No | No | No | No | Validation plans and a blinded experiment are documented; no external cohort, baseline result, adoption, payment, or market evidence exists. |

## Supported Configuration Matrix

This table describes source-visible construction paths, not a support SLA or operational certification.

| Surface | Source-visible current configuration | Boundary / qualification |
|---|---|---|
| Runtime | Rust 2024 workspace, Axum/Tokio process, synchronous SQLite | Locked workspace checks and isolated caches; no release matrix/deployment proof |
| API | HTTP on configured `listen_addr`; default `0.0.0.0:8100` | Bearer sessions, project membership and admin routes are implemented; not approved for public/shared deployment |
| Concierge LLM | `tj-provider` OpenAI-compatible path; Anthropic client code also exists | Configured default is Z.AI OpenAI-compatible `glm-5.1`; matrix not operationally proven |
| Stage LLM | ADK constructors for OpenAI, Anthropic, Gemini, and custom OpenAI-compatible providers | Only the configured Z.AI-compatible path has meaningful repository evidence; the provider matrix is not operationally verified, and ADK Git dependencies lack root `rev` pins |
| Chat | Zulip adapter present but refused at startup without mapped identity | Browser/Chief is the supported path; Zulip remains disabled |
| Workflow | JSON SMG plus per-stage JSON ADC/prompt files | Unknown fields are denied; run admission freezes the graph/ADC/prompt/schema/provider/model in an execution bundle |
| Native tools | `read_file`, `write_file`, `edit_file`, `bash`, enabled per ADC | File tools validate workspace paths; Bash fails closed unless the TJ-034 Landlock/seccomp boundary is available |
| Persistence | Local SQLite with WAL and migrations | Synchronous calls bounded to short transactions; lifecycle transitions, gate decisions, budgets and effects are transactional |
| Workspace | Optional local project workspace or remote repository; one task worktree/branch | No-repository runs are possible in parts of Manager; repository-free product path is not fully proven |
| Git provider | Forgejo only through the secrets-driven factory | Other provider types are rejected; legacy repo-file factory also remains in source |
| Git transport | Default git environment or Manager temporary SSH key for task clone/push | `StrictHostKeyChecking=no`; direct initial project clone does not use the guard |
| Secrets | Per-project SQLite values with AES-256-GCM storage at rest using `TURBOJET_MASTER_KEY` | ManagerHandle/SystemOperations removes a direct API path, not Bash environment/filesystem access; missing/invalid key disables the store; admin-gated routes and credential-safe logging exist; `tj-ops` backup/restore keeps ciphertext with an external key; rotation/complete audit controls remain incomplete |
| Logs | Tracing/JSONL components and HTTP log routes | Credential-safe diagnostic tests cover redaction; legacy reader behavior is not a support export |
| Execution isolation | In-process ADK and native tools in the TurboJET host context | Shell uses TJ-034 Landlock/seccomp, fixed environment and resource limits; no Docker/container or mount-namespace isolation |

## Historical Evidence Ledger

These records are historical. They were not rerun on 2026-07-20 and do not certify the current worktree.

| Date | Scope | Recorded result | Provenance | Evidence class / limit |
|---|---|---|---|---|
| 2026-05-23 | ADK-only migration sprint | Clean build and eight tests recorded | Local engineering record (2026-05-23) | Historical static evidence; Discovery QA was still pending in that sprint record |
| 2026-05-23 | Native `tj-tools` | 73 tests recorded | Local engineering record (2026-05-23) | Historical static evidence; not rerun for assessment |
| 2026-05-23 | Discovery regression after native tools | 9/9 checks recorded | Local engineering record (2026-05-23) | Partial historical scripted/cooperative operational evidence; lacks snapshot/configuration, raw artifacts, and operator metadata required by the current ledger and is not external validation |
| 2026-05-24 | Task-worktree redesign sprint | 104 tests plus clean build and Clippy recorded | Local engineering record (2026-05-24) | Historical static evidence; not proof of a complete real Forgejo lifecycle |

Historical TypeScript planning starts, predecessor PR-review systems, scenario documents, later secrets/worktree QA plans, and aggregate test counts are context, not direct static-test coverage or proof that the current Rust snapshot completes those lifecycles. Static-tested status requires test source that directly exercises the named capability. In particular, `tests/qa/scenarios/*.md` files prove that a scenario was designed unless a separate dated result proves it was executed.

## Known Limitations

- Current execution is trusted-host, in-process execution. `ShellTool` can run arbitrary Bash with inherited environment.
- Worktrees do not provide security isolation.
- ManagerHandle/SystemOperations separation removes a direct secret API path but is not an LLM security boundary; Bash inherits process environment and host filesystem access.
- Cancellation joins the flow future and shell process supervisors. Shell execution fails closed without the TJ-034 Landlock/seccomp boundary; it is not a container and remains unsuitable for deliberately hostile native code.
- Restart does not resume an exact model conversation; interrupted work requires an explicit retry that starts a new attempt.
- External publication is not one transaction with run state, but push/PR operations carry durable identities and `unknown` states that reconcile before retry; unpublished workspaces are retained.
- Run budgets cover questions/model/tool/correction/nudge/active time plus optional configured provider cost/call caps. Missing provider usage is explicitly `unknown`, not zero. Live provider cost/availability remains unproven (TJ-024/TJ-026 live).
- The supported output-schema vocabulary is intentionally limited; it is not full JSON Schema.
- Zulip remains disabled until authenticated account mapping exists.
- No SLO, alerting, remote CI, release, rollback, or supported deployment proof exists.
- Local HTTP Forgejo-protocol and bare-Git scenarios pass, but no real Forgejo/SSH pull request or reviewer evidence exists.
- Discovery evidence is scripted/cooperative; full-SDLC and commercial value are unvalidated.

## Prohibited Claims

Do not claim any of the following without a new ledger entry containing appropriate independent evidence:

- Docker isolation or container-isolated stages.
- Container-grade sandboxing or safety for hostile repositories, prompts, or users.
- Exact/deterministic restart recovery.
- Cancellation that terminates active flow tasks and all descendant processes.
- Enforced ADC/output schemas.
- A proven full Discovery -> Planning -> Engineering -> Delivery lifecycle on a real forge.
- A proven real Forgejo draft/ready/merge or SSH clone/push lifecycle.
- Production readiness, multi-tenant safety, deployment readiness, or operational reliability.
- Market validation, external user preference, adoption, willingness to pay, or reduced rework.
- That a scenario file was executed merely because it exists.
- That a historical test count passed today or applies unchanged to another snapshot.

## Verification Ledger Template

Append a row only after evidence has been collected and retained. Do not overwrite historical rows to imply recency.

| Verification date | Snapshot/config | Capability and claim | Evidence type | Procedure/scenario | Result | Artifact/provenance | Operator/reviewer | Limitations / expiry |
|---|---|---|---|---|---|---|---|---|
| YYYY-MM-DD | commit, lockfile, config IDs | Narrow falsifiable claim | static / operational / external | repository-relative path or runbook version | pass / fail / partial | logs, result file, PR, anonymized score sheet | names/roles | environment, sample, known gaps, rerun trigger |

For operational entries, include start/end time, environment, model/provider versions, external-service versions, cleanup result, and raw artifacts. For external validation, include participant selection, blinding/baseline method, scoring rubric, anonymization, and negative outcomes.

## Update Policy

1. Update the snapshot whenever source, configuration, manifests, or lockfile assumptions change.
2. Keep all five evidence columns independent; implementation never automatically upgrades testing or proof.
3. Record scenarios as Documented until a separate execution record exists.
4. Label every historical test with date, snapshot/provenance, and "not rerun" when applicable.
5. Require raw evidence for Operationally-proven and independent target-user outcome evidence for Externally-validated.
6. Downgrade stale or contradicted claims instead of preserving optimistic status.
7. Add supported configuration rows only for source-visible paths; call them certified only after explicit compatibility testing.
8. Keep product direction in [VISION.md](VISION.md), current internals in [ARCHITECTURE.md](ARCHITECTURE.md), and detailed audits in dated assessment documents.
9. Never use test counts, scenario presence, configuration breadth, or predecessor systems as substitutes for current end-to-end evidence.
