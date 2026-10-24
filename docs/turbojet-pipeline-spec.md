# TurboJET — Full SDLC Pipeline Specification

> Project Hub: Zettelkasten `20260503T194400802612000`
> Version: v4 (4 Phases, 17 Stages, 3 Parallel Streams)
> Last updated: 2026-05-03

---

## 1. Vision

TurboJET is a standalone system that orchestrates the complete Software Development Life Cycle with supervised autonomy — from initial customer contact through production monitoring.

**The problem**: ~60-80% of software project failures are requirements-related. Every existing AI dev tool (Devin, Copilot, Factory, Sweep, Codex) assumes crystal-clear requirements and only addresses the coding phase. Nobody handles the messy upstream work where projects actually succeed or fail.

**The differentiator**: TurboJET owns the full lifecycle, with AI-assisted Discovery and Planning as the primary value — the 80% of SDLC where projects fail but no tool addresses. Cross-stage orchestration (context propagation, feedback loops, adaptive pipelines) is the moat.

**The approach**: Supervised autonomy. The system runs freely within defined guardrails. Humans only review failures, anomalies, and decisions that fall outside confidence thresholds.

**Target user**: Engineering teams who want to augment their existing development process.

---

## 2. Pipeline Overview

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  PHASE 1: DISCOVERY — Initial                                               │
│    D1. INTAKE          Gather customer needs, artifacts                      │
│    D2. EVALUATE        Feasibility, alignment, general plan                  │
│                                              ← GATE: Go / No-Go             │
│                                                                              │
│  PHASE 2: PLANNING — Detailed (3 parallel streams + alignment)              │
│    ┌──────────────────┬──────────────────┬──────────────────┐               │
│    │  P1. ANALYZE     │  P2. ARCHITECT   │  P2b. DESIGN     │               │
│    │  (BA Stream)     │  (Arch Stream)   │  (Design Stream) │               │
│    │  Requirements    │  System design   │  UX/UI design    │               │
│    │  Epics/Stories   │  Tech stack      │  Wireframes      │               │
│    │  Acceptance      │  Integration     │  Mockups         │               │
│    │  criteria        │  Architecture    │  Design system   │               │
│    └────────┬─────────┴────────┬─────────┴────────┬─────────┘               │
│             └──────────────────┼───────────────────┘                          │
│                                ▼                                              │
│                        P3. ALIGN (3-way sync)                                │
│                                ▼                                              │
│                        P4. PLAN (sprint plan)                                │
│                                ▼                                              │
│                        P5. VALIDATE                                          │
│                            ← GATE: Backlog + Blueprint + Designs             │
│                                                                              │
│  PHASE 3: ENGINEERING (Sprint Execution)                                    │
│    Sprint loop per task:                                                     │
│    6. ANALYSIS → 7. DESIGN → 8. IMPLEMENT → 9. TEST → 10. REVIEW →         │
│    11. ACCEPTANCE                                                            │
│                            ← GATE: Story accepted                            │
│                                                                              │
│  PHASE 4: DELIVERY                                                          │
│    12. INTEGRATE → 13. RELEASE → 14. VERIFY → 15. OBSERVE                   │
│                                              ← GATE: Release complete        │
│                                                                              │
│  FEEDBACK LOOPS:                                                             │
│    TEST → IMPLEMENT          (failure context)                               │
│    REVIEW → IMPLEMENT        (findings context)                              │
│    ACCEPTANCE → DESIGN       (gap re-planning)                               │
│    VERIFY → RELEASE          (rollback)                                      │
│    OBSERVE → INTAKE          (production issues → new work items)            │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Phase 1: Discovery — Initial

### Purpose
Determine whether to pursue the work at all. Understand the customer's grand-level need, assess feasibility, and make a go/no-go decision.

### D1. INTAKE — Gather Customer Needs and Artifacts

**Who**: Account holder, marketing person, or any first point of contact.

**What happens**: Raw materials flow in from multiple channels.

**Input sources**:
- Meeting transcripts (Zoom, Teams recordings)
- Slack/Teams conversations (exported threads)
- Email threads
- Wireframes, sketches, hand-drawn diagrams (photos or digital)
- Partial documents (Google Docs, Notion, Confluence)
- Competitive analysis screenshots
- Customer support tickets
- Existing system documentation
- Voice memos, verbal notes
- Jira/ADO tickets (often vague or outdated)
- Figma files, design files
- RFP documents, proposal materials

**AI actions**:
- Ingest all artifacts regardless of format (text, images, audio, structured data)
- Normalize into a unified internal representation
- Extract key themes, entities, relationships
- Build an initial understanding map
- Identify gaps: "you mentioned X but never defined Y"
- Flag contradictions across sources

**Output**: Unified artifact corpus with extracted entities, themes, and gap analysis.

**Design note**: Multi-modal ingestion (audio, images, video) is the full vision. Text-only PoC for v1 is acceptable — the downstream Discovery stages work with text.

---

### D2. EVALUATE — Feasibility, Alignment, General Plan

**Who**: Director-level leadership evaluates. Customer may be involved for alignment.

**What happens**: Assess whether this is work the organization should take on.

**Evaluation dimensions**:

1. **Business Need Clarification**
   - What problem are we solving?
   - Who experiences this problem?
   - How do they solve it today?
   - What happens if we don't solve it?

2. **Business Model**
   - How does this create value?
   - Who pays? Why would they pay?
   - Revenue model (subscription, transaction, licensing, freemium)
   - Cost structure implications
   - Market positioning

3. **Feasibility Assessment (Director)**
   - Do we have the capacity and skills?
   - Is the timeline reasonable?
   - Is the budget aligned with expectations?
   - Do we have relevant experience/domain knowledge?
   - Are there technical showstoppers?

4. **Mutual Alignment (Customer + Director)**
   - Both sides agree on potential
   - Scope, budget, timeline expectations aligned
   - Contractual/commercial terms
   - Communication cadence agreed

5. **General Plan**
   - High-level approach to the work
   - Phases and milestones
   - Resource allocation
   - Risk register (initial)

**AI role**:
- Synthesize intake materials into a coherent picture
- Propose business model patterns ("this looks like a marketplace model")
- Analyze feasibility signals, flag risks
- Challenge assumptions (devil's advocate)
- Draft the general plan

**Output**: Business case document, feasibility assessment, general plan.

**Phase Gate: Go / No-Go**
- **Go**: Proceed to Planning phase. All three streams (BA, Architect, Design) begin.
- **No-Go**: Document reasons, archive for future reference.

**Human decision**: The go/no-go is ALWAYS a human decision. AI recommends, humans decide.

---

## 4. Phase 2: Planning — Detailed

### Purpose
Produce the three artifacts Engineering needs to execute without ambiguity:
1. **Product Backlog** (from BA) — what to build
2. **Technical Blueprint** (from Architect) — how to build it
3. **Visual Designs** (from Design) — what it should look like

Combined into a sprint plan that maps work to execution cadence.

### Three Parallel Streams

The three streams run in parallel and iterate. They converge at P3 (ALIGN) for synchronization.

---

### P1. ANALYZE — BA Stream

**Who**: Business Analysts.

**Goal**: Define every single detail of what needs to be built, from the business perspective.

**Activities**:

1. **Requirements Clarification**
   - Meet with customer stakeholders
   - Clarify business rules and workflows
   - Define integration requirements
   - Identify edge cases
   - Resolve contradictions from D1 intake

2. **User Stories and Acceptance Criteria**
   - Map user personas to workflows
   - Write user stories with happy paths and error paths
   - Define specific, testable acceptance criteria for each story
   - Cover edge cases and negative scenarios

3. **Business Rules Documentation**
   - State machines where applicable
   - Validation rules
   - Business invariants
   - Approval workflows

4. **Integration Requirements**
   - External systems to integrate with
   - Data contracts
   - API requirements (consumed and provided)
   - Authentication/authorization requirements

5. **AGILE Decomposition**
   - Epics → Stories → Tasks → Subtasks
   - Each task has expected output/deliverable
   - Dependencies between stories/tasks mapped

6. **Sprint Plan Structure**
   - Number of sprints
   - Sprint duration
   - Which stories in which sprint
   - Dependency ordering
   - Expected velocity

7. **Non-Functional Requirements**
   - Performance targets
   - Scale expectations
   - Security requirements
   - Accessibility needs
   - Compliance constraints

**AI role**:
- Suggest probing questions to ask stakeholders
- Spot gaps in requirements ("you defined the happy path but not what happens when payment fails")
- Draft user stories from informal descriptions
- Identify contradictions between requirements
- Propose acceptance criteria
- Estimate complexity from requirements

**Output**: Product Backlog — epics, stories, tasks, subtasks with acceptance criteria, sprint plan structure, integration requirements, NFRs.

---

### P2. ARCHITECT — Architecture Stream

**Who**: Technical Architects, Systems Architects (for large work).

**Goal**: Design the system to minute detail so no developer is left ambiguous on how to do the work.

**Activities**:

1. **System Architecture**
   - C4 model diagrams: context, container, component
   - Service/module boundaries
   - Communication patterns (sync, async, event-driven)
   - Deployment architecture

2. **Technology Stack Selection**
   - Language, framework, runtime for each component
   - Database selection (relational, document, graph, cache)
   - Infrastructure platform (cloud, on-premise, hybrid)
   - Third-party services and SaaS tools
   - Rationale for each choice

3. **Integration with Existing Systems**
   - Map existing codebases and frameworks
   - Identify reusable components and libraries
   - Define integration points and protocols
   - Account for existing knowledge bases
   - Migration strategy if replacing existing systems

4. **API Contracts**
   - OpenAPI / GraphQL schemas
   - Request/response formats
   - Error handling contracts
   - Versioning strategy

5. **Data Architecture**
   - Entity-relationship diagrams
   - Database schemas
   - Data flow diagrams
   - Migration plans
   - Caching strategy

6. **Security Architecture**
   - Authentication and authorization model
   - Data encryption (at rest, in transit)
   - API security
   - Audit logging

7. **CI/CD Pipeline Design**
   - Build pipeline
   - Test automation strategy
   - Deployment pipeline
   - Environment strategy (dev, staging, production)

8. **Architecture Decision Records (ADRs)**
   - Every significant decision documented
   - Context, decision, consequences
   - Alternatives considered

9. **Coding Standards**
   - Project-specific patterns and conventions
   - Code structure and naming conventions
   - Error handling patterns
   - Logging standards

**AI role**:
- Suggest architecture patterns based on requirements
- Generate C4 diagrams from descriptions
- Propose tech stack based on constraints
- Identify potential integration issues
- Validate architecture against NFRs
- Map existing codebases for reuse opportunities
- Draft API contracts from user stories

**Output**: Technical Blueprint — system architecture, tech stack, API contracts, data models, integration design, CI/CD design, ADRs, coding standards.

---

### P2b. DESIGN — Visual Design Stream

**Who**: UX/UI Designers (human) or AI-generated. Designs may come as ingress (provided externally) or be generated by the system.

**Dual mode**:
- **Ingress mode**: Customer or in-house designers provide Figma files, design systems, prototypes. The system validates and aligns them.
- **Generated mode**: No external designer. The system produces designs from BA requirements.
- **Hybrid mode**: Partial designs provided, system fills gaps.

**Activities**:

1. **UX Research**
   - User flows and journey maps
   - Persona validation
   - Usability considerations

2. **Wireframes** (low-fidelity)
   - Layout and structure
   - Navigation flow
   - Information architecture

3. **Visual Design** (high-fidelity)
   - Color system, typography, spacing
   - Component design
   - Responsive/adaptive layouts
   - Dark/light mode if applicable

4. **Interaction Design**
   - Animations and transitions
   - Micro-interactions
   - State designs (loading, error, empty, success)

5. **Design System / Component Library**
   - Reusable components
   - Design tokens (colors, spacing, typography as variables)
   - Pattern library
   - Accessibility specs (WCAG compliance)

6. **Prototypes**
   - Clickable prototypes for stakeholder validation
   - Key user flow demonstrations

7. **Asset Export**
   - SVGs, images, icons
   - Component specs for implementation

**AI role** (in generated mode):
- Generate wireframes from user stories
- Produce mockups from requirements + design system
- Create component specs
- Suggest interaction patterns
- Generate design tokens

**AI role** (in ingress mode):
- Validate provided designs against requirements
- Identify gaps (missing states, responsive breakpoints)
- Align designs with Architect's component architecture
- Flag inconsistencies between designs and stories

**Output**: Visual Designs — mockups, design system, prototypes, responsive layouts, accessibility specs, design tokens, assets.

**Key insight**: Design generates requirements, not just visuals. Seeing a mockup reveals missing edge cases, undefined states, and implicit needs. The design stream feeds back into BA and Architect streams.

---

### P3. ALIGN — Three-Way Synchronization

**Who**: BA, Architect, and Design leads converge. This is the critical sync point.

**What happens**: All three streams converge to resolve conflicts, fill gaps, and eliminate ALL ambiguity before Engineering starts.

**Conflict resolution matrix**:

| Conflict Type | Example | Resolution |
|---|---|---|
| Design ↔ BA | Mockup reveals missing requirement (empty state, error handling) | BA adds stories/acceptance criteria |
| Design ↔ Architect | Complex animation requires specific frontend tech | Architect adjusts tech stack or Design simplifies |
| BA ↔ Architect | Business requirement constrained by technical limitation | Negotiate scope or approach |
| All three | "This design is too complex for the timeline" | Scope negotiation across all streams |

**Tandem work patterns**:
- BA discovers integration requirement → Architect designs how, Design adds UI for it
- Architect finds existing framework → BA adjusts scope, Design works within its constraints
- BA defines user flow → Design maps to screens → Architect maps to components/APIs
- Design creates complex component → Architect validates technical feasibility → BA adjusts acceptance criteria

**AI role**:
- Maintain shared context between all three streams
- Detect contradictions across streams
- Propose resolutions
- Ensure nothing falls through the cracks

**Output**: Aligned, conflict-free Product Backlog + Technical Blueprint + Visual Designs.

---

### P4. PLAN — Sprint Plan

**Who**: PM/Leadership with input from BA and Architect.

**What happens**: Combine all three streams into an executable sprint plan.

**Activities**:
1. Feature prioritization (MoSCoW, impact vs. effort)
2. Sprint assignment (which stories in which sprint)
3. Dependency ordering within and across sprints
4. Resource allocation
5. Timeline estimation
6. Risk mitigation plans
7. Definition of Done for the sprint

**AI role**: Propose prioritization, estimate effort, identify dependency risks.

**Output**: Sprint Plan — sprint count, duration, story assignments, dependencies, milestones, resource plan.

---

### P5. VALIDATE — Stakeholder Sign-Off

**Who**: All stakeholders — customer, director, BA, architect, design lead.

**What happens**: Present the complete Planning output for review and approval.

**Activities**:
1. Walk through Product Backlog (what we're building)
2. Walk through Technical Blueprint (how we're building it)
3. Walk through Visual Designs (what it will look like)
4. Walk through Sprint Plan (when we're building it)
5. Confirm understanding matches intent
6. Final gap check
7. Feasibility confirmation
8. Sign-off

**AI role**: Present artifacts clearly, answer questions, flag remaining ambiguities.

**Phase Gate: Plan Approval**
- **Approved**: Proceed to Engineering. Sprint execution begins.
- **Rejected**: Back to the relevant Planning stream with specific feedback.

**Required artifacts for gate passage**:
- Product Backlog with acceptance criteria
- Technical Blueprint with architecture, tech stack, integration design
- Visual Designs with mockups, design system, prototypes
- Sprint Plan with assignments and dependencies
- Risk register

---

## 5. Phase 3: Engineering — Sprint Execution

### Purpose
Execute the sprint plan by implementing each story/task through the development lifecycle.

### Sprint Model
Engineering operates in sprints. Each sprint contains a set of stories from the backlog. Multiple stories can be in different stages simultaneously (story A in REVIEW while story B in IMPLEMENT). Dependencies are enforced by the sprint plan.

### Task Lifecycle
```
TODO → IN_ANALYSIS → IN_DESIGN → IN_PROGRESS → IN_TEST → IN_REVIEW → IN_ACCEPTANCE → DONE
                                                                    ↑                      │
                                                                    └── BLOCKED ←──────────┘
```

---

### Stage 6. ANALYSIS — Codebase Impact per Task

**What happens**: Before touching code, understand how this specific task affects the codebase.

**Activities**:
- Locate affected files/modules from the Technical Blueprint
- Understand existing code patterns and conventions (AGENTS.md, README)
- Check for existing similar implementations (reuse from blueprint mapping)
- Identify potential conflicts with other in-progress work
- Review the story's acceptance criteria against codebase reality
- Estimate task-level complexity

**AI role**: Codebase exploration, pattern matching, impact analysis.

**Inputs**: Task definition (from backlog), Technical Blueprint, existing codebase.
**Outputs**: Task-level impact map, affected files list, reuse opportunities, risk flags.

---

### Stage 7. DESIGN — Task-Level Technical Design

**What happens**: Translate the system-level blueprint into a concrete plan for THIS specific task.

**Activities**:
- Map the task to specific files, functions, APIs to create/modify
- Design data model changes for this task
- Define API contract changes for this task
- Plan integration with existing components
- Reference design mockups for UI tasks (which components, which layout, which interactions)
- Identify edge cases specific to this task
- Define the test plan for this task

**Distinction from P2 (Architect)**: P2 is system-level design. Stage 7 is task-level — the specific "how to implement this one story" level. P2 says "use React with a component library." Stage 7 says "for this story, create a new FilterPanel component using the existing Table pattern, modify the API endpoint to accept filter params, and add these 3 test cases."

**AI role**: Generate task implementation plan from blueprint + task definition + mockups.

**Inputs**: Task definition, Technical Blueprint, Visual Designs (for UI tasks), Analysis output (Stage 6).
**Outputs**: Task implementation plan (files to create/modify, changes per file, test plan).

---

### Stage 8. IMPLEMENT — Write Code

**What happens**: Write the actual code changes.

**Activities**:
- Create/modify files per task implementation plan
- Follow patterns from Technical Blueprint
- Follow design system for UI components
- Follow coding standards and conventions
- Handle edge cases identified in Stage 7
- Write inline documentation where needed

**AI role**: Primary coding agent via CodingTool interface (OpenCode CLI, OpenCode SDK, or other agent).

**Integration with existing codebase**:
- The CodingTool interface abstracts the specific coding agent
- CLI mode: wraps OpenCode CLI, Claude Code, Aider, or any CLI tool
- SDK mode: deep OpenCode integration (structured output, sessions, SSE events)
- Pipeline stages are backend-agnostic

**Inputs**: Task implementation plan, codebase context, Visual Designs.
**Outputs**: Code changes (git diff), change summary.

---

### Stage 9. TEST — Generate and Run Tests

**What happens**: Verify the implementation works correctly.

**Activities**:
- Generate unit tests for new/modified code
- Generate integration tests for API/data changes
- Generate UI tests for visual changes (compare against mockups)
- Execute test suite
- Analyze failures
- Measure coverage delta

**AI role**: Test generation, failure analysis, suggest fixes.

**Inputs**: Code changes, acceptance criteria, test plan from Stage 7.
**Outputs**: Test results, coverage report, failure analysis.

**Feedback loop**: Tests fail → back to Stage 8 (IMPLEMENT) with failure context. The AI receives error messages, failing test code, and original implementation to produce a fix.

**Max iterations**: Configurable retry limit. After N failed attempts, escalate to human.

---

### Stage 10. REVIEW — Code Quality Review

**What happens**: Review the code changes holistically across multiple dimensions.

**Review dimensions**:
- Code quality (readability, complexity, naming)
- Security (vulnerabilities, auth, data handling)
- Architecture alignment (does it follow the blueprint?)
- Design fidelity (does the UI match the mockups?)
- Convention compliance (project-specific rules)
- Performance (query efficiency, memory, caching)
- Edge case coverage
- Test quality (meaningful tests, not just coverage theater)

**Finding categories**:

| Category | Severity | Blocks merge? |
|---|---|---|
| Bug | High | Yes |
| Security | Critical | Yes |
| Performance | Medium | Case-by-case |
| Style | Low | No |
| Suggestion | Info | No |

**AI role**: Automated review.

**Existing implementation**: This IS the `pr-review` workflow from `sdlc-flows`. Already in production with Azure DevOps. Handles:
- Structured review findings (Bug, Security, Performance, Style, Suggestion)
- Reviewer vote casting (Approve / Approve with Comments / Changes Requested)
- Author reply monitoring and AI re-engagement
- Stale thread management and deduplication

**Inputs**: Code changes, test results, acceptance criteria, Visual Designs, Technical Blueprint.
**Outputs**: Review findings (categorized with severity), verdict (approve/request changes).

**Feedback loop**: Blocking findings → back to Stage 8 (IMPLEMENT) with review context.

---

### Stage 11. ACCEPTANCE — Verify Story Acceptance Criteria

**What happens**: Check if the implementation meets the story's acceptance criteria as defined by the BA.

**Activities**:
- Check each acceptance criterion defined in P1
- Verify visual fidelity against mockups (for UI tasks)
- Verify API contracts match blueprint
- Verify integration works end-to-end
- Verify non-functional requirements (performance, accessibility)
- Identify gaps between "done" and "accepted"

**AI role**: Trace acceptance criteria to implementation, identify gaps.

**Inputs**: Implementation, test results, review findings, acceptance criteria, Visual Designs.
**Outputs**: Acceptance status (pass/fail/partial), gap analysis.

**Feedback loop**: Gaps found → back to Stage 7 (DESIGN) to re-plan with gap context. Going back to DESIGN (not IMPLEMENT) because gaps may require rethinking the approach, not just re-coding.

**Phase Gate: Story Accepted**
- **Accepted**: Story moves to DONE. Eligible for Integration (Stage 12).
- **Rejected**: Back to Stage 7 with specific gap analysis.

---

### Sprint-Level Orchestration

**Parallelism**: Multiple stories in different stages simultaneously. Story A in REVIEW while Story B in IMPLEMENT. Orchestrator manages.

**Dependencies**: Story B depends on Story A. Sprint plan (P4) defines these. Orchestrator enforces execution order.

**Sprint board states**: TODO, IN_ANALYSIS, IN_DESIGN, IN_PROGRESS, IN_TEST, IN_REVIEW, IN_ACCEPTANCE, DONE, BLOCKED.

**Retrospective signals** (auto-detected):
- Frequent TEST failures → architecture may be brittle
- Frequent REVIEW failures → blueprint has gaps
- Frequent ACCEPTANCE failures → requirements are unclear
- Many feedback loop iterations → estimation problems
- Consistent BLOCKED state → dependency management issues

**Sprint velocity**: Track story points completed per sprint. Feeds back to Planning for future sprint estimation accuracy.

---

## 6. Phase 4: Delivery

### Purpose
Take accepted stories through integration, deployment, and into production monitoring.

### Release Cadence Options

| Strategy | When | Trigger |
|---|---|---|
| Per-story | Continuous deployment | Each story accepted (Stage 11) |
| Per-sprint | Sprint boundary | All sprint stories accepted |
| Per-release | Multiple sprints | Version milestone reached |
| Hotfix | Emergency | Critical production issue |

---

### Stage 12. INTEGRATE — Merge and Validate Combined State

**What happens**: Merge accepted changes into the target branch and validate the combined state.

**Activities**:
- Merge feature branch into target branch (develop or main)
- Resolve merge conflicts (AI-assisted with context from both sides)
- Run integration tests on merged state
- Validate no regressions from combined changes
- Run full CI pipeline on merged state
- Verify API contracts still hold across integrated changes
- Check for dependency conflicts between merged features

**AI role**: Conflict resolution suggestions, regression analysis.

**Inputs**: Merged code, CI results, integration test results.
**Outputs**: Merged branch, integration test results, conflict resolution report.

**Decision point**: Integration failure → back to Stage 8 (IMPLEMENT) for the conflicting story with context about what it conflicts with.

---

### Stage 13. RELEASE — Build, Package, Deploy

**What happens**: Take integrated code through the release pipeline.

**Activities**:
- Build artifacts (compile, bundle, containerize)
- Run full test suite on build artifacts
- Version bump (semantic versioning)
- Generate changelog from accepted stories
- Deploy to staging environment
- Run smoke tests on staging
- Deploy to production environment
- Run smoke tests on production

**Deployment strategies**:
- Blue-green (zero-downtime)
- Canary (gradual rollout)
- Feature flags (decouple deploy from release)
- Rolling deployment

**AI role**:
- Changelog generation from story metadata
- Deployment strategy suggestion based on change risk level
- Rollback decision support
- Post-deploy anomaly detection

**Autonomy boundary**:
- **Staging deployment**: Fully autonomous
- **Production deployment**: Human-confirmed (supervised autonomy)

---

### Stage 14. VERIFY — Post-Deployment Validation

**What happens**: Prove the deployment worked and the system is healthy.

**Activities**:
- Run smoke tests against production/staging
- Verify health endpoints responding
- Check key user flows end-to-end
- Compare metrics to pre-deployment baseline
- Verify no new error spikes
- Check database migration success
- Verify external integrations still work
- Check monitoring/alerting is active

**AI role**: Anomaly detection, baseline comparison, automated verification.

**Inputs**: Deployment, monitoring dashboards, test results, baseline metrics.
**Outputs**: Verification status (pass/fail), anomaly report.

**Rollback criteria**:
- Error rate exceeds threshold
- Key user flow broken
- Health check failures
- Performance degradation beyond threshold

**Decision point**: Verification fails → trigger rollback to Stage 13 (RELEASE) with failure context.

---

### Stage 15. OBSERVE — Continuous Monitoring and Feedback

**What happens**: Continuous monitoring of the deployed system. This stage runs perpetually, not just once.

**Activities**:
- Monitor error rates, latency, throughput
- Monitor resource usage (CPU, memory, disk, network)
- Monitor business metrics (user actions, conversion, revenue)
- Collect user feedback (support tickets, app store reviews, NPS)
- Detect regressions compared to baseline
- Detect anomalies (spikes, drops, unexpected patterns)
- Generate incident reports for anomalies
- Auto-create work items for production issues

**AI role**:
- Anomaly detection (statistical analysis of metrics)
- Root cause analysis (correlate errors with deployments, code changes)
- Auto-triage (classify severity, impact, urgency)
- Auto-create INTAKE items for discovered issues
- Predictive analysis (trending toward problems)

**The critical feedback loop — OBSERVE → INTAKE**:
- Production bug → D1 as a bug report
- User feedback → D1 as a feature request
- Performance degradation → D1 as a non-functional requirement
- Monitoring alert → D1 as an incident
- Business metric change → D2 as a business insight
- Anomaly detected → D1 with auto-collected context

This closes the full lifecycle loop. Every production observation can trigger a new Discovery cycle.

---

## 7. Adaptive Pipeline Paths

Not all work items pass through all 17 stages. The pipeline adapts based on the triage in D2.

| Item Type | Design Involvement | Path |
|---|---|---|
| New product (full) | Full design stream (ingress or generated) | D1→D2→P1+P2+P2b→P3→P4→P5→sprint(6→7→8→9→10→11)×N→12→13→14→15 |
| New feature (UI) | Mockups + prototypes | D1→D2→P1+P2b→P3→P5→sprint loop→12→13→14→15 |
| New feature (API/backend) | None or minimal | D1→D2→P1+P2→P3→P5→sprint loop→12→13→14→15 |
| Bug from production | None | D1→D2→6→8→9→10→11→12→13→14→15 |
| Typo / trivial fix | None | D1→6→8→9→10→12→13→14→15 |
| Hotfix | None | 6→8→9→10→12→13→14 (skip Discovery and Planning) |
| Refactor | None | D1→D2→P2→P5→6→8→9→10→12→13→14→15 |
| Documentation | None | D1→8→10→12 |
| UI polish | Design review only | D1→P2b→8→10→11→12→13→14→15 |
| Performance fix | None | D1→D2→P2→6→8→9→10→11→12→13→14→15 |

---

## 8. Confidence-Based Escalation (Supervised Autonomy)

Each stage produces a confidence score (0–1).

| Score | Action |
|---|---|
| 0.8 – 1.0 | Proceed autonomously |
| 0.5 – 0.8 | Proceed with human notification |
| 0.3 – 0.5 | Pause, request human input |
| 0.0 – 0.3 | Block, require human decision |

**Confidence signals by stage**:
- **D2**: Business case clarity, feasibility certainty
- **P1**: Requirements completeness, stakeholder alignment
- **P2**: Technical risk level, integration complexity
- **P2b**: Design completeness, responsiveness to requirements
- **P3**: Cross-stream alignment, unresolved conflicts
- **6–8**: Code complexity, ambiguity in implementation plan
- **9**: Test pass rate, coverage adequacy
- **10**: Review severity distribution, security findings
- **11**: Acceptance criteria pass rate, gap severity
- **12**: Integration conflict count, regression indicators
- **14**: Post-deploy health metrics vs. baseline

---

## 9. Existing Implementation Mapping

| Pipeline Stage | Existing Codebase | Status |
|---|---|---|
| D1–D2 | — | Not yet built |
| P1–P5 | — | Not yet built |
| 6. ANALYSIS | — | Not yet built |
| 7. DESIGN | — | Not yet built |
| 8. IMPLEMENT | `lib/opencode/invoke.sh` in sdlc-flows | CLI adapter exists, SDK adapter needed |
| 9. TEST | — | Not yet built |
| 10. REVIEW | `workflows/pr-review/` in sdlc-flows | **In production** (Azure DevOps) |
| 11. ACCEPTANCE | — | Not yet built |
| 12–15 | — | Not yet built |

**Existing codebases**:
- `~/Dev/sdlc-flows` — Bash framework (~1,900 lines). Extensible `lib/` + `workflows/` + `hooks/` architecture.
- `~/Dev/ado-review` — Earlier prototype (~1,000 lines). Active across 5 repos for inlooptech.

---

## 10. Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Autonomy level | Supervised | AI runs freely, escalates low-confidence decisions |
| Pipeline model | Gate-based, 4 phases, 17 stages | Clear decision points, adaptive paths |
| Discovery | First-class phase, initial + detailed | Business need before code — the differentiator |
| Planning | 3 parallel streams + ALIGN | BA, Architect, Design converge before Engineering |
| Engineering | Sprint loop, task lifecycle | AGILE-compatible, parallel story execution |
| Delivery | Staging auto, prod human-confirmed | Safety boundary for production deploys |
| Design | Ingress or generated, same gate | Flexibility for teams with/without designers |
| CodingTool | Hybrid (CLI + SDK) | Tool-agnostic CLI + deep SDK for OpenCode |
| Target user | Engineering teams | Higher value, more demanding |
| State management | Modular/pluggable | TBD: file-based, DB, or event-sourced |
| **Tech stack** | **TypeScript + Bun** | OpenCode SDK native, AI dev ecosystem alignment |
| Runtime | Bun (not Node.js) | Single binary compile, faster, built-in pkg manager |
| **State management** | **3-layer: DB + Events + Filesystem** | SQLite→PostgreSQL, Bun events→Redis, artifacts on disk |
| **Containerization** | **Docker per stage** | Sandboxed execution, 4 sandbox types (ro/rw/deploy/monitor) |
| **Project management** | **Redmine (GPL v2)** | Truly free, self-hosted, MCP support, battle-tested |
| **Git platform** | **Forgejo (community-governed)** | Fork of Gitea, truly free, lightweight, PR support, MCP |
| **Dogfooding** | Redmine + Forgejo for TurboJET dev | Test integrations by building TurboJET with TurboJET |

---

## 11. Tech Stack Decision

**Language**: TypeScript
**Runtime**: Bun (not Node.js)

### Rationale
1. OpenCode SDK is TypeScript-native — typed client, SSE streaming, structured output, session management
2. AI dev CLI tool ecosystem is TypeScript-based (OpenCode, Claude Code, Aider) — shared patterns and community
3. If performance becomes critical for specific components, Rust can be introduced later
4. AI models write excellent TypeScript — largest training corpus of any language

### Why Bun over Node.js
- Single binary compilation (`bun build --compile`) — solves deployment
- Faster runtime than Node.js
- Built-in package manager — no npm/yarn/pnpm fragmentation
- Native TypeScript support — no build step
- Better for CLI tooling

### Architecture Principles
- **Plugin-based**: each pipeline stage, integration, and adapter is a plugin
- **Interface-driven**: TypeScript interfaces define contracts between components
- **Modular**: each phase (Discovery, Planning, Engineering, Delivery) is an independent module
- **State machine**: typed orchestration engine managing stage transitions
- **Event-driven**: SSE/WebSocket for real-time pipeline monitoring

---

## 12. Toolchain Decisions

### Language & Runtime
- **TypeScript** — OpenCode SDK native, AI dev ecosystem alignment
- **Bun** (not Node.js) — single binary compile, faster, built-in package manager

### Infrastructure
- **Containerized**: Docker per pipeline stage, 4 sandbox types
- **State**: SQLite (start) → PostgreSQL (production), Bun events (start) → Redis (distributed)
- **API**: Containers communicate with orchestrator via REST API (orchestrator is single DB writer)

### Integrations (Dogfooding)
- **Redmine** (GPL v2) — Project management, issue tracking, MCP support
- **Forgejo** (community-governed) — Git hosting, pull requests, code review, MCP support
- **OpenCode** — AI coding agent via CLI + SDK (hybrid CodingTool interface)

All three are self-hosted, truly free, and used to manage TurboJET's own development.

---

## 13. Open Threads

1. ~~**Tech stack**~~ — Resolved: TypeScript + Bun
2. ~~**Project management**~~ — Resolved: Redmine
3. ~~**Git platform**~~ — Resolved: Forgejo
4. ~~**State management**~~ — Resolved: 3-layer (DB + Events + Filesystem)
5. ~~**Containerization**~~ — Resolved: Docker per stage
6. **Multi-modal ingestion (D1)** — Text-only PoC for v1, expand later
7. **Conversation engine (P3 ALIGN)** — How does the AI facilitate three-way sync?
8. **Sprint board / task state** — How to manage task lifecycle state?
9. **Monitoring integration (Stage 15)** — Which observability tools to integrate?
10. **Design generation (P2b)** — What AI capabilities for generating visual designs?
11. **Evolution path** — How to incrementally build from sdlc-flows to this full pipeline?
12. **Cost model** — LLM costs across 17 stages per work item; smart model routing
13. **Plugin architecture** — How stages/integrations register, discover, and execute
14. **Redmine + Forgejo deployment** — Set up instances for TurboJET development
