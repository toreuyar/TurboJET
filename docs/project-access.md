# Project-scoped access (TJ-006)

Owner decision D2, 2026-10-10: projects require explicit membership. This keeps
the earlier single project-member role; it does not introduce viewer/editor roles.

## Rights

| Operation | Project member | Instance administrator |
|---|---|---|
| List projects, lanes, runs | Member projects only | All |
| Read run tree, decisions, artifacts, files, inputs, activity | Member projects only | All |
| Start/cancel/delete runs, answer stages, decide gates, acknowledge merge, fork artifacts | Member projects only | All |
| Receive pipeline events and stage-log streams | Member projects only | All |
| Read shared workflow templates/model listing | Yes, authenticated | Yes |
| See workflow-to-lane associations | Member projects only | All |
| Create projects, pin/import workflows | No | Yes |
| Manage membership, accounts, secrets, SSH keys; system diagnostic logs | No | Yes |
| Read/delete another user's Chief conversation | No | No through Chief chat routes |

Workflow definitions are instance-wide administrator-managed templates. Project
association metadata is filtered. Project members can execute tools in their
projects; membership is an access boundary, not execution sandboxing (TJ-034).

## Migration and membership administration

`project_members(project_id, user_id)` is added without automatically granting
existing users access. Existing projects remain accessible to instance admins;
admins explicitly assign members. Parentless legacy runs are admin-only. Deleting
a project/account removes its memberships. Unknown/deleted users never acquire
rights from a caller-supplied admin flag.

Authenticated admin endpoints:

- `GET /v1/projects/{project_id}/members` → `{ "user_ids": [...] }`
- `PUT /v1/projects/{project_id}/members/{user_id}` → add membership, 204
- `DELETE /v1/projects/{project_id}/members/{user_id}` → remove membership, 204
- `POST /v1/admin/activity/prune` → bounded telemetry retention cleanup (never runs/artifacts/packets/secrets)
- `GET /api/logs/*` → redacted durable activity diagnostics

Use the stable account ID from `GET /v1/users`. Both membership mutations are
idempotent; authorization and membership writes share a database transaction.
Membership administration currently uses these endpoints rather than a webui form.

## Enforcement

The API's matched-route middleware authenticates private routes and rejects
unauthorized resource requests before invoking handlers. Route parameters are
decoded by Axum. Collection handlers filter project-derived rows; run creation
checks its project before target validation or creating work. Artifact operations
resolve authorization through the source run. Unclassified private routes fail
closed until explicitly added to the policy.

Chief receives a credential-scoped operations handle. Every tool call revalidates
the session and project membership, including calls after a model await. Denied
mutations cannot enqueue work or create records. Tool errors are returned rather
than claiming success. A handle without user credentials has no Chief privileges.

SSE authenticates its query token at connection time and repeatedly thereafter.
Pipeline events check membership before each delivery; stage-log streams check
session and membership on each poll. Revoked/expired sessions close with an `auth`
event; removing membership hides that project's subsequent events. Previously
delivered data is not recalled.

Chief server-session IDs encode `(user_id, chat_id)` as a tuple, avoiding separator
collisions. Browser Chief history and stage drafts are user-keyed; unowned legacy
storage is retained but not auto-imported or displayed. REST 401 clears local
session/query state; project denial (403) does not log out an otherwise valid user.

## Deployment and integration limits

The default bind address is loopback (`127.0.0.1:8100`). Explicit existing bind
configuration is preserved. The API uses same-origin browser access, including
the development proxy, instead of permissive cross-origin access. Remote
deployments should expose the configured service through their authenticated/TLS
deployment boundary. Session expiry remains 30 elapsed days from issuance.

Bootstrap creates exactly one initial administrator with a conditional database
insert. Later accounts are admin-created. Logout reports a persistence failure
instead of claiming that an unrevoked session was revoked.

**Legacy Zulip/chat configuration is unsupported under project-scoped access.**
It lacks an authenticated mapping between chat senders and TurboJET accounts.
Startup rejects configured chat adapters before starting the runtime; remove the
`chat` configuration and use webui/Chief. Restoring the optional adapter requires
explicit account mapping and authorization for both commands and stage answers.
No global internal handle is used as a substitute identity.

## Verification

Access regression checks cover cross-project read/mutation denial before handler
execution, decoded path IDs, privileged routes, membership/session revocation,
SSE filtering and revocation, Chief tool bypass attempts, bootstrap concurrency,
artifact/legacy-run boundaries, and conversation ID collisions. Frontend checks
cover 401 versus 403 handling and account-switch history isolation.

`cargo test --workspace --locked --offline` with the clean-cache environment
passes **304 tests**, no compiler warnings (11 added access regressions).
`npm test` passes **30 tests**; TypeScript/Vite production build passes.
All **6 mocked browser tests pass**, including account-switch history isolation.
`git diff --check` passes. Logs: `/tmp/opencode/turbojet-access-final.log`,
`/tmp/opencode/turbojet-access-ui-tests.log`, and
`/tmp/opencode/turbojet-access-browser-final.log`.
No migration was applied to a production database and no deployment was made.
