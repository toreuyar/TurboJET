# External CI/CD delivery integration (TJ-039/TJ-040)

**Status:** local contracts, API, UI, signed callbacks, disposable HTTP adapter
and deterministic drills implemented. **No real CI/CD, deployment target,
staging drill or rollback evidence exists here.** TJ-039 and TJ-040 remain
externally BLOCKED.

## Delegated target choice

The implementation delegates the single integration to a
**Forgejo-Actions-compatible external CI/CD system**. Forgejo Actions is chosen
because the existing repository path already speaks the Forgejo PR protocol and
the forge remains the owner of repository, runner, credential and deployment
target configuration. TurboJET does **not** run containers, execute deployment
steps, merge, mark a PR ready, or release anything.

The adapter boundary is generic (`tj_delivery::DeliveryAdapter`); replacing
Forgejo Actions later is a configuration/adapter change, not a change to the
identity, callback or recommendation contracts. This is a technical delegation
recorded for the owner, not a production target approval.

## Identity and provenance

Every release row is written **before** any external dispatch and carries the
exact:

- run, project and publication IDs;
- selected task, plan revision/hash and packet revision/hash;
- PR number/URL, branch, base SHA and commit SHA;
- environment (`staging`/`production`) and kind (`deploy`/`rollback`);
- expected revision (the publication commit, or the rollback target SHA);
- optional immutable production authorization ID and rollback source release;
- workflow file/ref and a deterministic operation key.

A second request with the same exact operation key returns the existing row and
does **not** dispatch again. A different identity is a different operation.

## State contract

| Field | Values |
|---|---|
| Dispatch | `pending`, `running`, `succeeded`, `failed`, `unknown` |
| CI | `not_started`, `pending`, `running`, `succeeded`, `failed`, `cancelled`, `unknown` |
| Deployment | `not_requested`, `pending`, `running`, `succeeded`, `failed`, `unknown` |
| Independent verification | `pending`, `passed`, `failed`, `unknown` |
| Observation | `pending`, `healthy`, `degraded`, `failed`, `unknown` |
| Recommendation | `pending`, `ready`, `blocked` |

Rules enforced in `tj_delivery::model` and covered by tests:

- A callback's revision must equal the exact expected revision. A mismatch is
  rejected before mutation.
- A stale or out-of-order sequence (`<= last_sequence`) is rejected and retained
  as an audit row; state does not move backwards.
- `event_id` is globally unique. A duplicate replays the first result; the same
  ID with a different payload/release is a conflict.
- Failed/cancelled CI, failed deployment, failed verification, degraded/failed
  observation, a failed dispatch or a revision mismatch makes the recommendation
  `blocked`.
- `unknown` is never success. It keeps the recommendation `pending` until an
  authenticated callback or explicit poll proves the result.
- `ready` requires a succeeded exact-revision CI (for deploy), succeeded exact
  deployment, passed independent verification with a non-trivial evidence hash,
  and healthy observation.
- Production dispatch requires the **latest** immutable authorization decision
  bound to the exact run/publication/kind/revision; a later rejection supersedes
  an earlier authorization, and a fresh decision is a new idempotent action. A
  staging deploy additionally requires a `ready` staging release for the same
  exact commit, with accepted verification and observation event IDs recorded in
  the decision.
- Rollback requires a successful production source deployment and a different
  exact target SHA authorized before dispatch. Rollback verification must report
  the target SHA, not the old deployment.
- Restart reconciliation marks in-flight dispatch/deployment/CI as `unknown`;
  it never infers success. A later signed callback or poll may resolve it.
  Re-dispatch is explicit, acknowledged, refuses existing external evidence or
  an external run ID, and reuses the immutable operation key; the workflow must
  deduplicate that key.

## HMAC callback contract

Endpoint: `POST /v1/delivery/releases/{release_id}/callbacks`

Headers:

- `X-TurboJet-Delivery-Timestamp`: RFC 3339 (or Unix seconds)
- `X-TurboJet-Delivery-Event`: event ID (1–200 printable characters)
- `X-TurboJet-Delivery-Signature`: `sha256=<hex HMAC-SHA256>`
- `X-TurboJet-Delivery-Source`: external source identity (optional if the body
  carries `source`)

Canonical signed text:

```text
<timestamp>\n<event_id>\n<sha256_hex(raw_body)>
```

The signature secret is `callback_secret_env`, minimum 16 bytes. Timestamps
outside `callback_window_secs` are rejected before any DB write. The signed
header event ID must equal the body `event_id`. Event IDs are the durable
replay nonce; `sequence` must increase per release.

Example body:

```json
{
  "event_id": "wf-901-ci-1",
  "sequence": 1,
  "source": "forgejo-actions",
  "type": "ci",
  "status": "succeeded",
  "revision": "<exact 40-char commit>",
  "external_run_id": "901",
  "external_url": "https://forge.example/actions/901",
  "workflow": "delivery.yml"
}
```

Callback types: `ci`, `deployment`, `verification`, `observation`. Deployment
and verification carry `environment`; verification carries a distinct
`verifier` identity and `evidence_hash`; observation carries `evidence_hash`.

## Configuration

```json
{
  "delivery": {
    "provider": "forgejo_actions",
    "base_url": "https://forge.example",
    "token_env": "TURBOJET_FORGE_TOKEN",
    "owner": "example",
    "repo": "product",
    "workflow": "delivery.yml",
    "git_ref": "main",
    "callback_secret_env": "TURBOJET_DELIVERY_CALLBACK_SECRET",
    "callback_window_secs": 300,
    "callback_base_url": "https://turbojet.example"
  }
}
```

A configured adapter with a missing token/secret, unsupported provider or
invalid URL fails startup closed. With no `delivery` block, history remains
inspectable but dispatch/polling/callbacks are disabled.

The external workflow must accept these dispatch inputs:
`turbojet_release_id`, `turbojet_run_id`, `turbojet_environment`,
`turbojet_kind`, `turbojet_commit`, `turbojet_expected_revision`,
`turbojet_operation_key`, `turbojet_callback_path`, `turbojet_callback_url`.
It should check out `turbojet_expected_revision`, run the project's tests,
deploy only to the named environment, verify the actual deployed revision and
health, then send signed callbacks. It must reject a reused operation key for a
different identity and must not merge or mark the PR ready.

## API / UI / Chief

Run-scoped, authenticated and project-authorized:

- `GET /v1/pipeline/runs/{id}/delivery` — releases, decisions, callback audit,
  recommendation reasons and integration limitations.
- `POST /v1/pipeline/runs/{id}/delivery/releases` — explicit staging release, or
  production release with `authorization_id`.
- `POST /v1/pipeline/runs/{id}/delivery/decisions` — immutable production
  authorize/reject decision; idempotency key required.
- `POST /v1/pipeline/runs/{id}/delivery/releases/{release_id}/reconcile` —
  explicit poll of the external CI run list; CI evidence only. Polling requires
  the exact durable `external_run_id` recorded at dispatch and the exact
  commit. A missing id or an unrelated run at the same commit is a visible
  conflict, never inferred success.
- `POST /v1/pipeline/runs/{id}/delivery/releases/{release_id}/retry-dispatch` —
  explicit acknowledged re-dispatch after failed/unknown dispatch.

The browser run view shows states, exact identities, recommendation reasons,
dispatch/CI/verification errors, decisions and accepted/rejected callbacks.
Chief has a read-only `get_delivery` tool. It cannot dispatch, authorize,
rollback, merge or release.

## Local evidence (not live deployment)

```sh
CARGO_HOME=/tmp/opencode/turbojet-clean-cargo \
CARGO_TARGET_DIR=/tmp/opencode/turbojet-clean-target \
  cargo test -p tj-delivery -p tj-db -p tj-api --offline
```

- `tj-delivery`: pure state/recommendation tests, HMAC replay-window tests, and
  a disposable HTTP Forgejo-Actions dispatch/poll test.
- `tj-db`: release identity/idempotency, duplicate/stale callback audit,
  failed-CI/unknown blocking, restart `unknown`, redispatch guard and immutable
  decision tests.
- `tj-api/tests/delivery_harness.rs`: real API + SQLite + HMAC callback path with
  a disposable HTTP adapter. It drills staging readiness, duplicate and stale
  callbacks, unauthorized production refusal, exact authorization, production,
  rollback readiness and a later unknown deployment that never becomes success.
- Browser unit/build checks cover the Delivery panel payload rendering.

These prove local contract logic only. They do not prove a real Forgejo Actions
runner, real deployment, real health probe, remote CI identity or rollback.

## Operator live drill (BLOCKED here)

See [`tests/qa/scenarios/delivery-live.md`](../tests/qa/scenarios/delivery-live.md).
Do not check TJ-039/TJ-040 or claim verified delivery until that drill has real
external run URLs, deployed revision evidence, independent health evidence, a
tested rollback and owner acceptance.
