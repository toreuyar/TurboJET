# Workflow authoring

TurboJET workflows are explicit JSON state-machine graphs (SMG files). The engine validates every graph and its referenced agents, prompts, schemas, and routes before registering it; provider/model selection and the full execution bundle are resolved and frozen at run admission. There is no filename inference, built-in graph, or implicit default flow.

Shipped workflows are **self-contained packages** under `workflows/`. Each package owns its graph, agent definitions, prompts, and output schemas. No shared agent tree or shared schema tree is shipped. The legacy `adc_dir` setting is inert for packages: it is consulted only by old `file`/`directory` graphs whose stages use bare agent ids.

Start from a shipped package rather than inventing a graph shape:

- `workflows/agent-loop/` — plan → act → check → result with a bounded revision loop.
- `workflows/clarify-produce-review/` — durable questions and an exact-artifact human gate.
- `workflows/research-synthesize-verify/` — local-document research with citation verification.
- `workflows/discovery/` — the repository-free Discovery package.

These examples are **experimental** unless individually marked otherwise. See [Generic repository-free workflows](generic-workflows.md) for their behavior and limits.

## 1. Create the package directory

```text
workflows/hello/
  workflow.smg.json
  agents/
    hello/
      agent.json
      prompt.md
  schemas/
    hello.json
```

Rules:

- A package is a directory containing **exactly one** `workflow.smg.json`, at its root. Nested or duplicate manifests are rejected.
- Optional `agents/<id>/agent.json` and `schemas/*.json` files hold the stage resources. Every referenced file must exist, be UTF-8 text, and be non-empty where content is required.
- Every asset must be a regular file named directly. Symlinks, absolute paths, `..`, and any reference that escapes the package are rejected.
- Limits: at most 200 assets and 1 MiB per text asset (the manifest counts as an asset).
- Package contents are imported into the registry. Editing or deleting the source package later does not change an existing registry version.

## 2. Add the graph file

Create `workflows/hello/workflow.smg.json`:

```json
{
  "version": 1,
  "name": "Hello Workflow",
  "maturity": "experimental",
  "description": "A minimal repository-free drafting workflow.",
  "entry": "hello",
  "exit": "hello",
  "limits": {
    "maxIterations": 1,
    "maxSteps": 3,
    "maxDepth": 2
  },
  "flow-order": ["hello"],
  "flows": {
    "hello": {
      "type": "flow",
      "entry": "draft",
      "exit": "draft",
      "stages": {
        "draft": {
          "agent": "hello",
          "interactive": false,
          "routes": {
            "success": "_exit"
          }
        }
      }
    }
  },
  "catalog": {
    "id": "hello",
    "label": "Hello Workflow"
  },
  "capabilities": {
    "schema_version": 1,
    "workspace": { "kind": "none" }
  }
}
```

Required graph rules:

- `version` must be `1`.
- `entry` must be the first `flow-order` entry and an executable flow.
- `exit` must be the last `flow-order` entry and either an executable flow or the final approval gate.
- Every flow must appear exactly once in `flow-order`.
- Flow and stage names are operator identifiers; behavior comes from routes and declared capabilities, not names.
- Limits must be positive. Retries/loops must be bounded.
- `catalog.id` is required for source import. Use lowercase ASCII letters, digits, `.`, `_`, or `-`, starting with a letter or digit.
- At most one imported catalog may set `"default": true`.
- Legacy `merge_strategy` and `pr_config` fields are recognized as inert metadata; they do not authorize a merge.

A stage `agent` value is either an agent id (`"hello"` for `agents/hello/agent.json`) or the exact package-relative file (`"agents/review/agent.json"`). A package graph never falls back to the shared `adc_dir`.

## 3. Add the agent definition

Create `workflows/hello/agents/hello/agent.json`:

```json
{
  "id": "hello-draft",
  "name": "Hello drafter",
  "prompt": "prompt.md",
  "schema": "schemas/hello.json",
  "max_output_retries": 3,
  "input": { "artifacts": [] },
  "output": {
    "artifacts": [
      { "name": "output.md", "required": true, "verify": "non-empty" }
    ]
  },
  "tools": { "read": true, "write": true, "bash": false, "edit": false },
  "timeout": 300
}
```

- `id` and `name` must be non-empty.
- `prompt` is required. In a package, a bare filename resolves relative to the ADC's directory; a value containing `/` resolves from the package root.
- `schema` is required for every stage. In a package it resolves from the package root first, then from the ADC's directory. It must constrain `properties.verdict` to a string enum whose values **exactly** match the stage's route keys.
- Declared tools are least-privilege. Start with read/write only. `bash` and `edit` are not implied, and shell execution still fails closed without a supported local boundary.
- `timeout` must be positive. `max_output_retries` bounds malformed-output correction and interactive nudges.
- `output.artifacts` can require and verify local files. `output.md` is a convention in the examples, not a hidden completion marker.

The prompt file `workflows/hello/agents/hello/prompt.md` must exist and be non-empty. State the task, the exact output contract, what evidence to use, and what to do when information is missing. Never tell the model to invent a verdict or approval.

## 4. Add the output schema

Create `workflows/hello/schemas/hello.json`:

```json
{
  "type": "object",
  "required": ["verdict", "summary", "content"],
  "properties": {
    "verdict": { "type": "string", "enum": ["success"] },
    "summary": { "type": "string" },
    "content": { "type": "string" }
  }
}
```

The supported validator is deliberately limited: object `required`, `properties`, `type`, and string `enum` constraints. It is not full JSON Schema. Unknown or unsupported schema features fail import rather than being silently ignored.

Both interactive and non-interactive stages must produce a structured result with a valid `verdict`. Interactive stages complete through the validated `verdict` tool call; non-interactive stages return parsed JSON whose verdict drives routing.

## 5. Register the package source

Add the package to the operator config:

```json
"workflow_sources": [
  { "id": "hello", "path": "workflows/hello", "kind": "package" }
]
```

The three source kinds are:

| Kind | Behavior |
|---|---|
| `file` | One exact legacy `*.smg.json` graph. Agent/prompt/schema references use config-base paths, or the legacy `adc_dir` for bare ids. |
| `directory` | Scans **non-recursively**, sorted by path. Imports regular files ending in `.smg.json` and immediate subdirectories that contain a root `workflow.smg.json` as packages. Deeper graphs/packages must be configured separately. |
| `package` | Exactly one package directory with one root `workflow.smg.json`. All references resolve strictly inside that package. |

The shipped `config/turbojet.json` and `config/turbojet.selfhosted.json` load the four packages through:

```json
"workflow_sources": [
  { "id": "examples", "path": "workflows", "kind": "directory" }
]
```

A config with `workflow_sources: []` (or no field) is valid and starts with an empty registry. That state is degraded, not ready: the service, UI, and first-admin flow work, but project creation fails with actionable guidance until a workflow is registered.

Startup validates and imports every configured source atomically. A configured source that is missing or invalid fails startup; it is not silently ignored. You can re-scan already configured sources without changing the source list:

```sh
curl -sS -X POST http://127.0.0.1:8100/v1/workflows/import \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{}'
```

The import endpoint never accepts an arbitrary filesystem path. Add the package to a configured source first and restart after changing the source list.

## 6. Registry versions and immutability

On import, the workflow version and all package assets are written in one transaction. Assets are immutable per version:

- Editing a prompt, schema, or ADC does not change an existing registry version.
- Re-importing the same graph with different assets fails instead of silently ignoring the change. Change the graph content (for example bump its `version` or description) so a new immutable revision is created, then re-import.
- Run admission builds the execution bundle from stored registry assets only, with no ADC/prompt/schema filesystem reads. You can edit or delete the source package after import and later runs still use exactly the imported bytes.
- Startup still fails if a **configured** source is missing. Remove or repoint a deleted package in `workflow_sources` before restarting.
- Legacy `file`/`directory` graphs keep their disk resolution and create no asset rows.

Inspect the registry through `GET /v1/workflows` or the UI **Workflows** view. Each entry reports `source_kind` (`file`, `directory`, or `package`) and `asset_count` alongside the existing fields; `/v1/profiles` is unchanged. Create a project by `workflow_id` (exact immutable version) or `catalog_id` (latest imported revision). Lanes stay pinned to an exact workflow version; re-importing creates a new immutable revision rather than mutating an existing lane.

## 7. Gates and capabilities

A gate is a manager-owned human checkpoint, not a stage:

```json
"review": {
  "type": "gate",
  "routes": {
    "approved": "_next-flow",
    "rejected": "_rework"
  }
}
```

Gate rules are strict: exactly `approved` and `rejected`; approved targets `_next-flow`, rejected targets `_rework`; gates have no stages, entry, or exit. A gate binds the latest immutable artifact by default. Capabilities can bind a requirements packet or plan instead.

The optional `capabilities` block declares behavior that the engine must not infer:

- `workspace.kind`: `none` or a repository workspace contract.
- `gates`: exact review binding kind per gate.
- `requirements_packet` and `task_plan`: produce/validate/gate modules.
- `publication`: repository flow, draft-PR mode, required evidence, and task selection.
- `verification`: declared verification policy.
- `external_delivery`: optional external CI/CD handoff.

`schema_version` is required whenever `capabilities` is present. The supported version is `1`; unknown fields, versions, module names, and kinds fail validation. If the entire block is omitted, the engine uses the explicit generic defaults (isolated non-Git workspace, artifact-bound gates), not business inference.

## 8. Local full-SDLC package

The full SDLC graph is not shipped and is not a built-in. If an operator has the private, Git-ignored `config/local/full-sdlc.smg.json`, derive a self-contained package and configure it explicitly:

```sh
bash scripts/import-local-sdlc.sh derive
bash scripts/import-local-sdlc.sh configure --config config/turbojet.local.json
# Restart the service so the new source is loaded, then:
TJ_API_BASE=http://127.0.0.1:8100 TJ_API_TOKEN=... \
  bash scripts/import-local-sdlc.sh import-pin --lane <lane-id>
```

`derive` writes `config/local/full-sdlc.package/` (graph, agents, prompts, and schemas), verifies the original graph hash, and never modifies the original. `configure` adds one explicit `workflow_sources` package entry. `import-pin` imports the configured source and pins each named lane to the exact returned `workflow_id`. Everything under `config/local/**` is excluded from packages.

## 9. Validate before sharing

At minimum:

```sh
# Validate manifests and compile the core.
cargo check --workspace --locked

# Run the optional shipped-example contract suite after adding an example.
bash scripts/check-examples.sh
```

The default core suite intentionally removes shipped workflow/agent/schema/example assets:

```sh
bash scripts/check-asset-free.sh
```

A new graph should also be exercised through a disposable run with a scripted or disposable provider before anyone treats it as supported. Do not add live credentials to tests or commit generated run artifacts, logs, workspaces, or `.env` files.

## Authoring checklist

- [ ] Package has exactly one root `workflow.smg.json` and no escaping or symlinked assets.
- [ ] Graph declares a unique `catalog.id` and a truthful `maturity`.
- [ ] Every stage references an existing package agent, prompt, and schema.
- [ ] Every schema's verdict enum exactly matches its stage's route keys.
- [ ] Every route is bounded; no unbounded loop or silent fallback exists.
- [ ] Tools are least-privilege; shell/edit are enabled only with a reviewed reason.
- [ ] Gates bind an exact immutable artifact, packet, or plan.
- [ ] Paths and examples contain no machine-specific locations or secrets.
- [ ] Package graphs reference only files inside their package; no `adc_dir` fallback is assumed.
- [ ] The source list is explicit; the workflow is not selected by filename.
- [ ] Local/private packages stay under `config/local/` and outside public examples.
