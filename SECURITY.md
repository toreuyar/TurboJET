# Security policy

## Supported versions

TurboJET **1.0.0** is the current source release. No version is currently under a production support commitment, and there is no guaranteed security-response SLA.

The only code expected to receive security fixes is the current `main`/`master` branch at the latest source release. Older snapshots, private forks, operator-local graphs under `config/local/`, and historical documentation are not supported products. A version number is not a production-readiness or external-validation claim.

## Reporting a vulnerability

Do not open a public issue, discussion, or pull request containing exploit details, credentials, private data, or a working attack against a deployed instance.

This checkout does not configure a public GitHub owner, security email address, or private reporting address. Use the private channel actually published by the repository owner:

1. If the GitHub repository has **private vulnerability reporting** enabled, use **Security → Report a vulnerability** for that repository.
2. Otherwise, contact the maintainer through the private contact method published on their repository profile or project page.
3. If no private channel is published, ask the repository owner to establish one before sharing sensitive details.

A useful report includes:

- affected revision/tag and configuration shape (with secrets removed);
- deployment model: local, loopback, trusted LAN, or something else;
- prerequisites and minimal reproduction steps;
- observed impact and whether it requires authentication, project membership, or administrator rights;
- relevant logs or traces with secrets, tokens, cookies, hostnames, and personal data redacted;
- whether the issue is already public and any suggested embargo.

Never include live credentials. If a credential may have leaked, revoke or rotate it immediately; deleting a commit is not sufficient.

## Security model and known limitations

TurboJET is a **trusted-host, local/development system**, not a hardened multi-tenant platform.

- **No production approval.** The default tracked development config can bind `0.0.0.0:8100`; deployments are not approved for public internet or shared untrusted use. CORS is permissive, and bearer sessions plus project membership are not a complete multi-tenant isolation model.
- **Trusted execution.** Stage model/tool work runs inside the TurboJET process. Repositories, prompts, and inputs should be treated as trusted unless a separate containment design is in place.
- **Shell boundary, not a sandbox.** On supported Linux hosts, model-driven shell commands fail closed unless a local Landlock/seccomp boundary with filesystem/network allow-lists and resource limits is available. This is a narrow local boundary, not a container, mount namespace, VM, or guarantee against deliberately hostile native code. Worktrees are not security boundaries.
- **Secrets at rest.** Project secrets are encrypted with AES-256-GCM using `TURBOJET_MASTER_KEY` (64 hex characters). The key is external and must be backed up separately. A missing or invalid key disables secret functionality; it is not a recovery mechanism. The secret API boundary does not prevent model-driven tools from observing inherited environment or host filesystem data if configured with those permissions.
- **Authentication.** The first registration becomes an instance administrator; subsequent registration is closed. Bootstrap a fresh instance promptly and use a strong password. There is no published claim of external authentication-provider integration, MFA, or full audit logging.
- **Workflow content is executable policy.** Workflows can declare tools and gates. Review imported graphs, agent definitions, prompts, schemas, repository URLs, and delivery targets before running them. Only operators should control `workflow_sources`.
- **Data durability.** SQLite and artifact recovery are designed for explicit interruption/retry semantics, but exact model-conversation resume and transactional external publication are not implemented. Back up offline using the procedures in the [runbook](docs/runbook-self-hosted.md).
- **External services.** Provider, Zulip, Forgejo/SSH, and delivery integrations are optional and not externally validated here. Zulip is refused at startup without authenticated account mapping. Real forge/SSH publication and real deployment/rollback evidence are BLOCKED.
- **Logging and diagnostics.** Diagnostic logging redacts credential-bearing headers, cookies, query strings, bodies, and common secret patterns by regression test, but logs and stage activity can still contain sensitive user content. Restrict access, set retention deliberately, and sanitize support exports.
- **No security certification.** No independent penetration test, supply-chain audit, fuzzing campaign, or production incident process is claimed.

## Before public launch

This checkout is not yet published to a public remote. The owner must close these gaps first:

- **Repository URL and reporting channel.** Add the real public GitHub URL and enable private vulnerability reporting (or publish a private security contact). This file intentionally contains neither an invented URL nor an unpublished address.
- **Historical credential response.** The publishable branch is a single sanitized root commit, so earlier development commits are not reachable from it. That is history replacement, not credential revocation or physical erasure: the provider API key and Zulip bot credential that appeared in development history must still be revoked or rotated by their owners. Local reflogs, clones, and operator archives can retain the old objects and must not be published.
- **CI evidence.** `.github/workflows/checks.yml` is present, but no external CI run is claimed. Run the workflow on the public remote and record the result before a release or public launch.
- **Artifact boundary.** Local OpenCode tool/runtime state is preserved outside Git under an ignored archive; do not add it, `.env`, databases, logs, or generated reports to the public repository.

## Operator checklist

Before exposing an instance beyond localhost:

- use the loopback self-hosted template unless a reviewed network/reverse-proxy design exists;
- keep `TURBOJET_MASTER_KEY`, provider keys, tokens, `.env`, databases, logs, and backups outside version control and world-readable paths;
- bootstrap the first administrator immediately and review project membership;
- install only reviewed workflow sources and agent tools;
- do not enable shell/edit tools for untrusted inputs;
- set activity retention and review what support exports contain;
- stop the service before offline backup and verify restores in a disposable location;
- treat local scripted evidence as development evidence, not a production security proof.

See [CAPABILITIES.md](CAPABILITIES.md) and the [runbook](docs/runbook-self-hosted.md) for the detailed limitations.
