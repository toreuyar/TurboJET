# TurboJET Vision

> **Productization update (2026-10-10):** the proposed release vision retained the full-SDLC ambition and made Discovery the first finished release. The text below preserves the earlier vision and its decision history.

This document states intended direction, not current capability. Current implementation is described in [ARCHITECTURE.md](ARCHITECTURE.md); [CAPABILITIES.md](CAPABILITIES.md) separately records implementation, direct-test, operational-proof, and external-validation status. The review basis is committed source at `master@2cf96d4` plus explicitly identified uncommitted documentation and evidence; this document is not necessarily part of that committed snapshot.

## Direction

TurboJET's long-term direction is a supervised, evidence-driven full-SDLC orchestrator that preserves intent and traceability from an ambiguous request through planning, engineering, and delivery. It should coordinate specialized work, bounded retries, artifacts, and human decisions rather than present one unconstrained coding agent as an autonomous organization.

The direction is deliberately broader than the currently proven product. Planning, Engineering, Delivery, secure execution, deterministic recovery, and production operation remain vision outcomes unless [CAPABILITIES.md](CAPABILITIES.md) separately records stronger implementation, test, operational-proof, and external-validation evidence.

## Target Users and Job

The initial target users are product owners, founders, business analysts, engineering leads, agencies, and small teams that receive vague or contradictory software requests and lack dependable analysis capacity.

The near-term job-to-be-done is:

> When a stakeholder brings an ambiguous software request, expose omissions, assumptions, constraints, acceptance criteria, contradictions, and unresolved decisions, then produce a reviewable packet that can be approved and handed to planning or engineering with less rework.

The longer-term job is to preserve that approved intent across the rest of delivery without removing human authority at consequential boundaries.

## Desired Lifecycle Outcomes

### Discovery

Convert an ambiguous request into a portable, reviewable packet containing the problem, users, goals, non-goals, functional and non-functional requirements, testable acceptance criteria, constraints, dependencies, risks, confirmed facts, unsupported assumptions, contradictions, open decisions, and approval history.

Discovery should work without repository access. Its value must stand on the quality of the resulting decision packet, not on infrastructure breadth.

### Planning

Transform an approved Discovery packet into traceable work items, dependencies, design constraints, risk controls, and a feasible plan. Planning should retain unresolved decisions rather than silently invent answers, and a human should approve the plan before repository-changing work begins.

### Engineering

Implement a bounded, reviewable task in a task-scoped workspace; produce tests and evidence; expose failures and uncertainty; and create a durable draft change for human review. Before untrusted execution, separately test and establish process, filesystem, network, credential, and privilege isolation. Human merge authority remains mandatory.

### Delivery

Integrate with deployment, verification, rollback, and observation systems only when safety, reliability, and demand justify it. Production release authority remains human. Native deployment ownership is an option, not an assumed endpoint; integration with existing CI/CD may remain the preferred boundary.

## Principles

- **Supervised at consequential boundaries:** Humans approve requirements, plans, merge, and production release. Automation may recommend; it must not silently assume authority.
- **Discovery before infrastructure:** Validate that the system improves request quality before expanding forges, runners, deployment, or full-pipeline breadth.
- **Evidence before claims:** Source presence, test source, historical execution, current execution, and external validation are different evidence classes.
- **Fail visibly:** Missing contracts, ambiguous outcomes, failed durable writes, and unavailable dependencies should become explicit states rather than hidden success.
- **Portable artifacts:** Users should retain useful Markdown and machine-readable artifacts independent of TurboJET runtime state.
- **Traceability:** Later decisions and changes should point back to confirmed intent and named approvals.
- **Bounded autonomy:** Limit iterations, cost, time, tools, side effects, and scope; escalate when limits or confidence boundaries are reached.
- **Security is a boundary, not a label:** Worktrees are not sandboxes. Shell/code execution requires an explicit, tested isolation model before use with untrusted inputs.
- **Human authority survives automation:** Human merge and production release authority are product invariants.
- **Measured evolution:** Runner, provider, workflow, and architecture changes require observed user or reliability evidence, not framework preference.

## Discovery-First Validation Wedge

The first product hypothesis is repository-free supervised Discovery. It should remove Forgejo, SSH, worktrees, shell, and deployment from the critical path and export a versioned requirements packet.

The hypothesis is falsifiable: for real ambiguous software requests, TurboJET Discovery should produce materially more complete and no less grounded packets than a capable general-purpose LLM using a concise structured interview prompt, at acceptable time and interaction cost.

### Blinded Baseline Experiment

The intended first experiment is:

1. Recruit five external participants across product/founder, engineering-lead, and analyst/delivery roles.
2. Collect two real unresolved requests per participant, for ten requests total.
3. Compare repository-free TurboJET Discovery with a same-model concise structured-interview baseline where feasible.
4. Normalize packet formatting and present blinded Packet A/Packet B outputs to at least two independent reviewers.
5. Score critical omissions, unsupported assumptions, contradictions, requirement completeness, acceptance-criteria testability, NFR coverage, faithfulness, actionability, time, rounds, cost, retries, and participant effort.
6. Freeze prompts and code during scored runs; record manual intervention and failures.
7. Unblind only after scoring and publish the anonymized result, including a negative result.

The assessment proposes continuation thresholds of at least 30% fewer critical omissions, no increase in unsupported assumptions, median completion within 15-20 minutes and three rounds, four of five participant preferences, and two concrete pilots. Whether to adopt all of those thresholds is an open user decision, not a decision made by this document.

## Evidence-Gated Roadmap

Each stage starts only when the preceding evidence gate is met. Roadmap placement does not mean implementation or proof.

### Stage 1: Repository-Free Discovery Packet

- Portable Markdown plus versioned machine-readable output.
- Explicit fact, assumption, contradiction, risk, and open-decision separation.
- Participant approval, revision history, interruption recovery, and export.
- Gate: blinded baseline advantage, external preference, and concrete pilot commitments.

### Stage 2: Narrow Business-Analysis Planning

- Convert approved packets into stories, acceptance criteria, dependencies, NFRs, and retained decisions.
- Preserve evidence links from planned work to Discovery.
- Require a human plan gate.
- Gate: external users use the plan downstream with fewer clarification cycles or less rework.

### Stage 3: One-Task Discovery-to-Draft-PR

- Preserve the Manager-owned one-task worktree and branch across flows, secrets-backed per-project provider resolution, the ephemeral SSH-key guard, and human merge authority.
- One supported repository/forge shape, one task, one branch, one durably pushed commit, one durable draft PR; no deployment.
- Fix initial-clone SSH-key use, require host-key verification, and define remote push/PR durability and failure semantics.
- Select and separately test process, filesystem, network, credential, and privilege isolation before untrusted shell-enabled execution.
- Gate: durability, cancellation, restart/reconciliation, failure-injection, and external task-success evidence.

### Stage 4: Selective Engineering Expansion

- Add analysis, implementation, test, and review loops only for observed failure modes.
- Measure task success, review correction, escaped defects, cost, cancellation reliability, and intervention.
- Expand language/repository support only in response to pilot demand.
- Gate: repeated external use with acceptable safety and reliability.

### Stage 5: Delivery Integration or Deferral

- Prefer existing CI/CD, release, rollback, and monitoring integrations initially.
- Reconsider native release/verify/observe ownership only after repeated demand and mature controls.
- Preserve human production-release authority.

## Non-Goals and Deferred Scope

Until Discovery validates and current safety/reliability blockers close, the following are deferred rather than promised:

- Autonomous merge or production release.
- Native deployment, rollback, and production observation.
- Multiple forge providers beyond a proven narrow path.
- Parallel sprint, multi-task, or unattended full-SDLC orchestration.
- Additional runner or agent-framework migrations.
- Broad multimodal intake, visual design generation, and Penpot integration.
- Redmine ownership or broad project-management synchronization.
- Enterprise identity, tenancy, policy, compliance, and hostile-code execution.
- Additional general-purpose coding tools without measured Engineering demand.
- Public/network deployment of the current API.

## Open User Decisions

No option is selected here.

| Decision | Options | Principal trade-off |
|---|---|---|
| Near-term promise | Discovery-only product; continue full-SDLC positioning | Credible falsifiable wedge versus broader ambition with insufficient evidence |
| Discovery repository model | Repository-free default; require repository attachment | Lower burden/attack surface versus richer context and infrastructure coupling |
| Review style | Adaptive respectful challenge; mandatory adversarial evaluator | Trust/completion versus consistently aggressive assumption testing |
| Engineering execution | Disable shell until a sandbox exists; trusted single-user mode with explicit warning | Delayed Engineering versus narrow pilots without a security boundary |
| Validation bar | Adopt all assessment thresholds; use lower exploratory thresholds | Strong sunk-cost control versus more learning with weaker evidence |
| Baseline | Same-model concise prompt; best available analyst/tool process | Isolate orchestration value versus compare with the customer's strongest alternative |
| Forge scope | Forgejo-only until one-task proof; add major forges now | Fewer variables versus easier recruitment and broader integration burden |
| Delivery ownership | External CI/CD indefinitely; native Delivery after Engineering proof | Lower operational risk versus lifecycle control and major reliability cost |
| Recovery model | Exact checkpoint resume; explicit idempotent replay/reconciliation | Deterministic checkpoints versus potentially simpler replay with strict idempotency |
| Documentation history | Archive obsolete documents; rewrite them as current direction | Preserve evidence versus reduce file count without erasing history |
