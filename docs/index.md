# Documentation Index

> **Public entry point:** start with the [README](../README.md). TurboJET is a v1.0.0 source release of a self-hosted workflow engine at local/development maturity. No production, hosted-service, real-forge, live-deployment, or external-validation claim is made.

## Documentation

- [Introduction and concepts](introduction.md) — what TurboJET is, the data model, what is optional, and its security posture.
- [Setup](setup.md) — build the Rust service and browser UI, configure a workflow source, bootstrap the first administrator, and start a job.
- [Configuration](configuration.md) — top-level config, environment and `.env` loading, workflow sources, providers, budgets, delivery, and readiness.
- [Workflow authoring](workflow-authoring.md) — self-contained package layout, graph, catalog, agent definition, prompt, output schema, routes, gates, source kinds, and registry immutability.
- [Generic repository-free workflows](generic-workflows.md) — shipped experimental templates, limits, and integration evidence.
- [Architecture](../ARCHITECTURE.md) and [architectural principles](../ARCHITECTURAL_PRINCIPLES.md) — system design and historical narrative.
- [Capabilities and evidence](../CAPABILITIES.md) — separate implementation, static-test, operational-proof, and external-validation status.
- [Self-hosted runbook](runbook-self-hosted.md) — package layout, readiness, offline backup/restore, retention, and shutdown drills for a local/development host.
- [Reproducible checks](checks.md) — supported toolchain, core asset-free versus optional suites, CI jobs, and browser prerequisites.
- [Project access](project-access.md), [requirements packet](requirements-packet.md), [structured task plan](task-plan.md), [lifecycle contract](lifecycle-contract.md), [cancellation and timeouts](cancellation-timeouts.md).
- [External delivery integration](delivery-integration.md) — optional Forgejo-Actions-compatible contract; real deployment/rollback evidence is BLOCKED.
- [Live scenario preparation](../tests/qa/scenarios/discovery-live.md) — prepared supported-model journey; no live claim.
- [Evaluation protocol](../evaluation/README.md) — prepared blinded protocol and collection templates; no participants or scores.
- [Candidate release notes (HOLD)](releases/discovery-beta-candidate.md) — local evidence and BLOCKED external gates; not a release.
- [Contributing](../CONTRIBUTING.md), [security policy](../SECURITY.md), and [MIT license](../LICENSE).

## Core versus optional layers

- **Core:** the Rust engine/API, durable lifecycle, gates, questions, budgets, recovery, and browser UI. The default test suite is asset-free and does not require shipped workflow, agent, schema, or example files.
- **Optional shipped packages:** self-contained workflow packages under `workflows/`, each with its own graph, agents, prompts, and schemas. The shipped templates load them through a `directory` source; a config with no sources starts empty. The three generic templates are experimental; Discovery is the historical supported repository-free example.
- **Local-only full SDLC:** an operator-private, Git-ignored original graph under `config/local/` and its derived self-contained package. It is experimental, absent from a fresh clone/package, built with `scripts/import-local-sdlc.sh`, and never selected by filename.
- **Optional integrations:** Zulip is refused at startup until authenticated account mapping exists; Forgejo/SSH publication and external delivery have local scripted/mock evidence only, with real operation BLOCKED. Docker is not the current runtime.

Historical documents that still say **Chef** (or older **Concierge**) refer to the persona now named **Chief**; dated architecture records are preserved without rewriting. The dated `docs/turbojet-*.md` design records and `tests/qa/scenarios/interactive-verdicts.md` are historical provenance only: they may name removed shared config paths and are not setup or authoring instructions. Use [workflow authoring](workflow-authoring.md) for the current package layout.
