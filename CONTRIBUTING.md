# Contributing to TurboJET

TurboJET is a v1.0.0 source release at local/development maturity. Contributions are welcome, but the bar is explicit behavior, honest evidence, and a small reviewable scope. It is not production-ready, and documentation must not imply otherwise.

## Scope

Good contributions include:

- bug fixes in the Rust core, API, UI, persistence, and lifecycle contracts;
- tests for observable behavior, failure paths, and regressions;
- documentation corrections, setup improvements, and broken-link fixes;
- workflow examples that are clearly marked experimental and do not become implicit defaults;
- security hardening with a clear threat model and regression coverage.

Before a large refactor or a new workflow/runtime abstraction, open an issue describing the problem, the boundary, and the evidence you expect. Avoid mixing unrelated cleanup, formatting, dependency churn, and behavior changes in one pull request.

The project deliberately keeps these boundaries:

- no built-in workflow, default SDLC graph, or filename/profile inference;
- no silent fallback when a workflow, route, schema, artifact, or dependency is missing;
- no automatic merge, PR-ready transition, or production release;
- no claim that a worktree or local shell boundary is a container sandbox;
- no external validation claim without traceable evidence.

## Development setup

Follow [Setup](docs/setup.md). The supported baseline is Rust 1.98.0, Node 22.23.2, npm 10, and Linux. Install locked dependencies rather than regenerating them:

```sh
cargo build --locked -p tj-api
(cd webui && npm ci)
```

Create an ignored `.env` or export credentials locally. Never commit real keys, `.env`, `TURBOJET_MASTER_KEY`, tokens, private keys, runtime databases, logs, workspaces, or generated test reports.

## Checks

Use the narrowest check that covers your change, then run the relevant broader layer:

```sh
# Core Rust suite. It is asset-free and must not require shipped examples.
bash scripts/checks.sh rust

# Locked web install, unit tests, production build, and mocked browser checks.
bash scripts/checks.sh web

# Disposable copy with all shipped workflow/agent/schema/example assets removed.
bash scripts/check-asset-free.sh

# Optional shipped examples and, when available locally, the private SDLC regression.
bash scripts/check-examples.sh
```

Notes:

- The optional example suite may report the private local SDLC regression as `BLOCKED` when `config/local/full-sdlc.smg.json` is absent. That is expected; do not create a fake local graph or weaken the check to make it pass.
- Browser and backend suite counts are local/scripted evidence, not live-provider proof.
- Do not add a test that requires a paid call, a real external service, or a developer's credentials. Use disposable local fixtures and the existing scripted-model harnesses.
- Do not regenerate `Cargo.lock` or `webui/package-lock.json` unless the change explicitly requires a dependency update.
- Do not run destructive cleanup against a live state directory or an unpublished worktree.

If a check cannot run in your environment, say exactly what was not run and why in the pull request. Do not present an unrun check as passing.

## Pull requests

Keep each pull request focused and describe:

1. **What changed and why.** Name the user-visible or contract-level behavior.
2. **Scope and boundaries.** Call out anything intentionally unchanged, especially workflow selection, gates, routing, publication, or security behavior.
3. **Verification.** List the exact commands actually run and their results. Include a failing-path test when the fix concerns failure handling.
4. **Documentation.** Update current docs when behavior, setup, configuration, or claims change. Preserve dated historical records rather than rewriting them to look current.
5. **Public exposure.** Remove machine-specific paths, private hostnames, screenshots with credentials, logs, and generated artifacts.

The repository pull-request template mirrors this. A large generic checklist is not a substitute for scoped verification.

## Issues

Use the bug or feature issue template. Include a minimal reproduction, the exact revision/configuration shape (with secrets removed), and the commands/results you observed. Do not paste:

- API keys, bearer tokens, cookies, private keys, or `.env` contents;
- raw databases, secret exports, or support archives containing customer data;
- private repository URLs, hostnames, or machine-specific paths when a placeholder is enough.

For security vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.

## Secrets and private data hygiene

- Use placeholder values in examples: `https://forge.example`, `replace-with-token`, `/path/to/turbojet`.
- Keep local operator configs under `config/local/` (Git-ignored) or outside the repository.
- Treat logs and stage activity as potentially sensitive even when redaction is enabled.
- Test with disposable `TURBOJET_HOME`, workspaces, and databases.
- If you accidentally expose a credential, revoke/rotate it immediately; removing the commit is not sufficient.

## Licensing

TurboJET is MIT-licensed; see [LICENSE](LICENSE). By submitting a contribution, you agree that it may be distributed under the same license. Do not add third-party code, assets, fonts, or generated content unless its license is compatible and its provenance is recorded.

## Collaboration

Be precise, review the actual code and evidence, and critique the change rather than the person. Do not invent maintainers, response-time promises, or enforcement contacts.
