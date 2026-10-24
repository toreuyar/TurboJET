# Configuration reference

TurboJET loads one JSON configuration file per process. The file is strict: unknown fields are rejected rather than ignored. Path resolution, environment overrides, workflow admission, and readiness are all part of the startup contract.

Start with `config/turbojet.selfhosted.json`. Create an ignored operator copy as described in [Setup](setup.md).

## Top-level fields

| Field | Required | Meaning |
|---|---:|---|
| `listen_addr` | No | HTTP bind address. Default `127.0.0.1:8100`; the tracked development template uses `0.0.0.0:8100`. |
| `log_level` | No | Default tracing filter level (`debug` in debug builds, `info` in release). `RUST_LOG` overrides the full filter. |
| `database_path` | No | SQLite path. Default `$TURBOJET_HOME/state.db`. `$TURBOJET_HOME` is expanded. |
| `workspace_base` | No | Root for run/project workspaces. Default `.turbojet`; the self-hosted template uses `$TURBOJET_HOME/workspaces`. |
| `adc_dir` | No | Legacy shared agent root, consulted only by `file`/`directory` graphs with bare agent ids. Empty by default. Package workflows never use it. |
| `workflow_sources` | No | Explicit list of workflow graph or package sources (`file`, `directory`, `package`). Empty/absent means an empty registry; the shipped templates load `workflows/`. |
| `smg_path` | No | Deprecated single-file alias for `workflow_sources`. Do not use for new setups. |
| `providers` | No | Model provider definitions. Stage execution needs a usable provider. |
| `agents` | No | Conversational agent definitions, normally Chief. |
| `chat` | No | Optional chat transport. Zulip is currently refused at startup without authenticated mapping. |
| `run_timeout_secs` | No | Optional wall-clock limit for new runs, including human waits. Omitted keeps per-attempt watchdogs. |
| `budgets` | No | Run-wide question/model/tool/correction/active-time/cost limits frozen into new execution bundles. |
| `activity_retention_days` | No | Retention for replayable stage activity. Default `30`; `0` disables automatic pruning. Runs, artifacts, decisions, packets, and secrets are not pruned by this setting. |
| `delivery` | No | Optional Forgejo-Actions-compatible external delivery adapter. Omitted disables dispatch. |

Relative paths resolve from the **config base directory**, which is the parent of the directory containing the config file. For the shipped `config/turbojet.selfhosted.json`, the base is the repository root, so `path: "workflows"` means `<repository>/workflows`. A copy at `config/turbojet.local.json` has the same base. A config placed in a deeper directory has a different base: for example, `config/local/turbojet.json` would use `config/` as its base, so `path: "workflows"` would resolve to `config/workflows` and fail. Keep operator configs directly under `config/`, or use absolute paths deliberately.

## Environment and `.env`

TurboJET reads process environment values first, then the first existing `.env` at:

1. `<config-base>/.env` (for the shipped layout: repository root), or
2. `$TURBOJET_HOME/.env`.

A process environment value wins over the file. The file is simple `KEY=value` syntax; quotes around a value are stripped. This is not shell evaluation.

Recommended local variables:

```sh
TURBOJET_HOME=/absolute/path/to/state
TURBOJET_WEBUI_PATH=/absolute/path/to/webui/dist/index.html
ZAI_API_KEY=...                 # named by providers[].api_key_env
TURBOJET_MASTER_KEY=...         # 64 lowercase hex characters
```

Never place secret values in tracked JSON. Readiness reports presence, not values. Logs redact credential-bearing headers, query strings, cookies, bodies, and common secret shapes; that is not a guarantee that arbitrary user-provided content is safe to publish.

Provider overrides exist in addition to the API-key variable. For a provider with `id: "zai"`, the loader recognizes `TURBOJET_PROVIDER_ZAI_BASE_URL`, `TURBOJET_PROVIDER_ZAI_MODEL`, and `TURBOJET_PROVIDER_ZAI_API_KEY`. The Chief model can be overridden with `TURBOJET_AGENT_CHIEF_MODEL`; the legacy `TURBOJET_AGENT_CHEF_MODEL` is accepted only during the bounded compatibility window with a warning. Do not set both.

`start.sh` is a development helper that sources `config/.env` if it exists. The service's own loader uses the paths above. Prefer explicit environment management or an ignored root `.env`.

## Workflow sources

The shipped templates use one directory source for the four self-contained packages:

```json
"workflow_sources": [
  { "id": "examples", "path": "workflows", "kind": "directory" }
]
```

A mixed operator setup can name sources explicitly:

```json
"workflow_sources": [
  { "id": "workflows", "path": "workflows", "kind": "directory" },
  { "id": "legacy", "path": "legacy/my-flow.smg.json", "kind": "file" },
  { "id": "team-review", "path": "workflows/team-review", "kind": "package" }
]
```

- `id` is a stable operator identifier; it must be unique. If omitted, it is derived from the path.
- `path` is absolute or base-relative. `$TURBOJET_HOME` is expanded.
- `kind: "file"` must name one legacy graph file. Its agent/prompt/schema
  references use config-base paths, or the legacy `adc_dir` for bare agent ids.
- `kind: "directory"` scans **non-recursively**, sorted by path, and imports
  regular files ending in `.smg.json`. It also imports immediate subdirectories
  that contain a root `workflow.smg.json` as workflow packages. Nested graph
  files and deeper package directories must be configured separately.
- `kind: "package"` names exactly one package directory containing one root
  `workflow.smg.json`. Package agent/prompt/schema references resolve strictly
  inside that directory; there is no shared-`adc_dir` fallback.
- A configured source that is missing or invalid fails startup. It is not silently ignored.
- Startup imports every source. `POST /v1/workflows/import` re-scans configured sources; it does not accept an arbitrary path. Restart after changing the source list.
- Source import validates the graph, every stage's agent/prompt/schema reference, and the catalog block before registering anything. Provider/model selection is resolved and frozen at run admission.
- Package import persists the workflow version and its assets in one transaction.
  Package admission reads those stored assets, not the source directory. Legacy
  file/directory graphs keep their disk resolution and create no asset rows.

A workflow without a `catalog` block is rejected with an actionable error. At most one imported catalog may declare `"default": true`. `GET /v1/workflows` reports each version's `source_kind` (`file`, `directory`, or `package`) and `asset_count`. See [Workflow authoring](workflow-authoring.md) for package layout, reference rules, limits, and the local full-SDLC package workflow.

The full SDLC package is **not** a built-in or shipped asset. If an operator has the private, Git-ignored `config/local/full-sdlc.smg.json`, `scripts/import-local-sdlc.sh derive` builds `config/local/full-sdlc.package/` and `configure` adds one explicit package source. It is experimental and excluded from packages.

## Providers and agents

Example provider:

```json
"providers": [
  {
    "id": "zai",
    "type": "open_ai",
    "api_key_env": "ZAI_API_KEY",
    "base_url": "https://api.z.ai/api/coding/paas/v4",
    "default_model": "glm-5.1",
    "rate_limit_retries": 3,
    "transient_retries": 2,
    "retry_base_ms": 250
  }
]
```

Supported provider type names are `open_ai`, `anthropic`, `gemini`, and `custom`. The configured Z.AI-compatible path is the only one with meaningful repository evidence; other constructors are not an operational support claim.

- `api_key_env` names an environment variable, never the key itself.
- `default_model` is required. `base_url` has an empty default in the schema, but a usable OpenAI-compatible deployment normally needs an endpoint value.
- Optional `input_cost_micros_per_million` and `output_cost_micros_per_million` enable cost accounting/caps. Zero or absent price means unknown, not free.
- Retry budgets are per class. Credential/request errors are not retried; tool calls are not replayed for a transport retry.

Chief/agent configuration names a provider, model, enabled tools, and system prompt. The shipped canonical id is `chief`. Tool names are capability declarations for the conversational agent; they do not grant filesystem or arbitrary shell access by themselves. Keep the configured tool list aligned with what the UI/operator expects.

## Budgets

```json
"budgets": {
  "question_rounds_per_stage": 6,
  "question_rounds_per_run": 12,
  "model_calls": 120,
  "tool_calls": 200,
  "output_corrections": 24,
  "interaction_nudges": 12,
  "active_secs": 14400,
  "cost_micros": 0
}
```

Budgets are frozen into each new execution bundle. Retries and rework do not reset them. `cost_micros: 0` means no monetary cap is configured; it does not mean measured cost is zero. Missing provider usage is reported as `unknown`.

## Optional external delivery

Delivery is disabled unless a `delivery` block exists. It delegates execution to a Forgejo-Actions-compatible external workflow; TurboJET never performs the deployment itself.

```json
"delivery": {
  "provider": "forgejo_actions",
  "base_url": "https://forge.example",
  "token_env": "DELIVERY_TOKEN",
  "owner": "replace-with-owner",
  "repo": "replace-with-repository",
  "workflow": "replace-with-workflow-file",
  "git_ref": "main",
  "callback_secret_env": "DELIVERY_CALLBACK_SECRET",
  "callback_window_secs": 300,
  "callback_base_url": "https://turbojet.example"
}
```

All values above are placeholders. Use environment-variable names for credentials, not values. Real CI/CD, deployment, health verification, and rollback remain unproven; see [External delivery integration](delivery-integration.md) and the [BLOCKED live drill](../tests/qa/scenarios/delivery-live.md).

## Optional Zulip chat

A `chat` block can name a Zulip server, bot email, API-key environment variable, and default stream. It is not a supported onboarding path: the adapter is refused at startup until authenticated user/account mapping exists. Do not treat configuration alone as enabling a safe cross-transport identity model. Browser/Chief is the supported local surface.

## Readiness and degraded states

- `blocked` — database/schema, workspace write, or provider-key dependency failed.
- `degraded` — secrets master key missing/invalid, built UI missing, or workflow registry empty.
- `ready` — configured dependencies are present and at least one workflow is registered.

An empty registry is a valid, explicitly supported startup state but not a ready product. `POST /v1/projects` fails clearly until a workflow is installed. The shipped templates register the `workflows/` packages, so a normal source checkout starts ready for workflows; only a config with no sources (or a package built without `workflows/`) starts empty.

## Related documents

- [Setup](setup.md)
- [Workflow authoring](workflow-authoring.md)
- [Self-hosted runbook](runbook-self-hosted.md)
- [Project access policy](project-access.md)
- [Cancellation and timeouts](cancellation-timeouts.md)
- [Checks and verification](checks.md)
