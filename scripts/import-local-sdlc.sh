#!/usr/bin/env bash
# Explicit operator migration for the ignored local SDLC workflow.
#
# The original config/local/full-sdlc.smg.json is never modified. `derive`
# turns it into a self-contained package (graph + agents + prompts + schemas)
# with a `catalog` id and declarative `capabilities`. The package, like the
# original graph, is ignored by git and never shipped. No filename, profile or
# catalog inference pins a lane: a lane is pinned by the exact workflow_id
# returned by the registry import.
#
# Usage:
#   bash scripts/import-local-sdlc.sh derive
#   bash scripts/import-local-sdlc.sh configure [--config PATH]
#   TJ_API_BASE=http://127.0.0.1:8100 TJ_API_TOKEN=... \
#     bash scripts/import-local-sdlc.sh import-pin --lane LANE_ID [--lane ...]
#
# `configure` adds an explicit `workflow_sources` package entry. Restart the
# service afterwards so the configured source is loaded, then run `import-pin`.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
original="$root/config/local/full-sdlc.smg.json"
package="${TJ_SDLC_PACKAGE:-$root/config/local/full-sdlc.package}"
asset_source="${TJ_SDLC_ASSET_SOURCE:-}"
schema_source="${TJ_SDLC_SCHEMA_SOURCE:-}"
config="${TJ_CONFIG:-$root/config/turbojet.json}"
expected_hash="a88bed13195f925f9af01169ae9b438e3e161cdbc05257443569a25530f5d34f"

fail() { echo "error: $*" >&2; exit 1; }

derive() {
  [[ -f "$original" ]] || fail "original local SDLC graph is missing: $original"
  local actual
  actual="$(sha256sum "$original" | awk '{print $1}')"
  [[ "$actual" == "$expected_hash" ]] || fail "original local SDLC hash changed: $actual"
  python3 - "$original" "$package" "$asset_source" "$schema_source" <<'PY'
import json, shutil, sys
from pathlib import Path

src, package, asset_source, schema_source = map(Path, sys.argv[1:5])
doc = json.loads(src.read_text())

catalog = {
    "id": "full-sdlc",
    "label": "Full SDLC (local)",
    "description": "Local derived capability package; the original graph remains unchanged",
}
capabilities = {
    "schema_version": 1,
    "workspace": {"kind": "repository", "provisioning": "optional"},
    "gates": {"discovery-gate": "packet", "planning-gate": "plan"},
    "requirements_packet": {
        "produce_flow": "discovery",
        "producer_stage": "packet",
        "binding_gate": "discovery-gate",
    },
    "task_plan": {
        "produce_flow": "planning",
        "validate_stage": "validate",
        "binding_gate": "planning-gate",
        "source_packet": True,
        "task_selection": {"after_gate": "planning-gate", "required": True},
    },
    "publication": {
        "flow": "engineering",
        "mode": "draft_pr",
        "requires_task_selection": True,
        "required_evidence": [
            {"role": "test", "stage": "test", "execution": "shell", "criterion_mapping": True},
            {"role": "review", "stage": "review"},
            {"role": "acceptance", "stage": "acceptance"},
        ],
    },
    "external_delivery": {"after_flow": "engineering", "explicit_request_only": True},
}

def agent_id(reference: str) -> str:
    path = Path(reference)
    if len(path.parts) > 1:
        return path.parent.name
    return path.name

def copy_agent(agent: str) -> None:
    target_dir = package / "agents" / agent
    legacy_dir = asset_source / agent if str(asset_source) else Path()
    legacy_adc = legacy_dir / "agent.json"
    if not str(asset_source) or not legacy_adc.is_file():
        if (target_dir / "agent.json").is_file():
            return
        raise SystemExit(
            f"agent assets for '{agent}' are absent: the package does not contain "
            f"{target_dir / 'agent.json'} and no legacy asset source was supplied. "
            "Restore the package or set TJ_SDLC_ASSET_SOURCE/TJ_SDLC_SCHEMA_SOURCE."
        )
    adc = json.loads(legacy_adc.read_text())
    prompt = adc.get("prompt")
    if not prompt:
        raise SystemExit(f"agent '{agent}' declares no prompt in {legacy_adc}")
    prompt_path = Path(prompt)
    legacy_prompt = prompt_path if prompt_path.is_absolute() else legacy_dir / prompt_path
    if not legacy_prompt.is_file():
        raise SystemExit(f"prompt for agent '{agent}' is missing: {legacy_prompt}")
    target_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(legacy_prompt, target_dir / prompt_path.name)
    if prompt_path.name != prompt:
        adc["prompt"] = prompt_path.name

    def move_schema(value: dict) -> None:
        reference = value.get("schema")
        if not reference:
            return
        schema_name = Path(reference).name
        legacy_schema = schema_source / schema_name
        if not legacy_schema.is_file():
            raise SystemExit(
                f"schema '{reference}' for agent '{agent}' is missing: {legacy_schema}. "
                "Restore it or set TJ_SDLC_SCHEMA_SOURCE."
            )
        (package / "schemas").mkdir(parents=True, exist_ok=True)
        shutil.copyfile(legacy_schema, package / "schemas" / schema_name)
        value["schema"] = f"schemas/{schema_name}"

    move_schema(adc)
    status = adc.get("output", {}).get("status")
    if isinstance(status, dict):
        move_schema(status)
    (target_dir / "agent.json").write_text(json.dumps(adc, indent=2) + "\n")

agents = sorted({
    agent_id(stage["agent"])
    for flow in doc.get("flows", {}).values()
    for stage in flow.get("stages", {}).values()
})
for agent in agents:
    copy_agent(agent)
    for flow in doc.get("flows", {}).values():
        for stage in flow.get("stages", {}).values():
            if agent_id(stage["agent"]) == agent:
                stage["agent"] = agent

doc["catalog"] = catalog
doc["capabilities"] = capabilities
package.mkdir(parents=True, exist_ok=True)
(package / "workflow.smg.json").write_text(json.dumps(doc, indent=2) + "\n")
print(package)
PY
  echo "local SDLC package written; original hash unchanged: $actual"
}

configure() {
  [[ -f "$config" ]] || fail "config file is missing: $config"
  python3 - "$config" "$package" <<'PY'
import json, sys
from pathlib import Path
path, package = Path(sys.argv[1]), Path(sys.argv[2])
doc = json.loads(path.read_text())
sources = doc.setdefault("workflow_sources", [])
entry = {"id": "local-full-sdlc", "path": str(package), "kind": "package"}
for source in sources:
    if source.get("id") == entry["id"]:
        source.update(entry)
        break
else:
    sources.append(entry)
path.write_text(json.dumps(doc, indent=2) + "\n")
print(f"configured workflow source local-full-sdlc -> {package}")
PY
  echo "restart the service, then run: $0 import-pin --lane <lane-id>"
}

api() {
  local method="$1" path="$2" body="${3:-}"
  [[ -n "${TJ_API_BASE:-}" ]] || fail "TJ_API_BASE is required"
  [[ -n "${TJ_API_TOKEN:-}" ]] || fail "TJ_API_TOKEN is required"
  if [[ -n "$body" ]]; then
    curl -fsS -X "$method" "$TJ_API_BASE$path" \
      -H "Authorization: Bearer $TJ_API_TOKEN" \
      -H 'Content-Type: application/json' --data "$body"
  else
    curl -fsS -X "$method" "$TJ_API_BASE$path" \
      -H "Authorization: Bearer $TJ_API_TOKEN"
  fi
}

import_pin() {
  local lanes=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --lane) lanes+=("$2"); shift 2 ;;
      *) fail "unknown import-pin argument: $1" ;;
    esac
  done
  [[ ${#lanes[@]} -gt 0 ]] || fail "import-pin requires at least one explicit --lane LANE_ID"
  api POST /v1/workflows/import '{}' >/tmp/turbojet-sdlc-import.json
  local workflow_id
  workflow_id="$(python3 - /tmp/turbojet-sdlc-import.json <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
items = doc.get("workflows") or []
matches = [w for w in items if w.get("catalog_id") == "full-sdlc"]
if not matches:
    raise SystemExit("catalog 'full-sdlc' was not registered; check the configured source and restart")
latest = max(matches, key=lambda w: (w.get("revision", 0), w.get("version", 0)))
print(latest["id"])
PY
)"
  echo "registered full-sdlc workflow_id=$workflow_id"
  for lane in "${lanes[@]}"; do
    api PUT "/v1/lanes/$lane/workflow" "{\"workflow_id\":\"$workflow_id\"}" >/dev/null
    echo "pinned lane $lane -> $workflow_id"
  done
}

command="${1:-}"
shift || true
case "$command" in
  derive) derive ;;
  configure) configure ;;
  import-pin) import_pin "$@" ;;
  *) fail "usage: $0 {derive|configure|import-pin --lane LANE_ID ...}" ;;
esac
