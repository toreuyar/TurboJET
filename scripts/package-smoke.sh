#!/usr/bin/env bash
# Fresh-package smoke in disposable directories: copy the versioned package,
# start its binary with its built UI, check readiness and stop with SIGTERM.
# Never installs into a system path and never calls a live provider.
#
# Three modes prove the distribution boundary:
#   empty      - no workflow sources/assets at all: HTTP/UI serve, readiness is
#                degraded for workflows, the catalog is empty, and project
#                creation is refused with actionable import guidance.
#   shipped    - the package's own `workflows/` packages are retained and
#                loaded by the packaged config: every shipped catalog imports,
#                registry assets are stored, and a project/lane pins the exact
#                discovery workflow version.
#   synthetic  - one exact synthetic package is injected into the disposable
#                copy: the catalog imports, a project/lane pins its workflow id,
#                and no shipped example or local asset is required.
#
# Usage: bash scripts/package-smoke.sh [PACKAGE_DIR] [empty|shipped|synthetic|both|all]
# `both` is the historical empty+synthetic pair; `all` adds the shipped mode.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
package="${TJ_PACKAGE_DIR:-/tmp/opencode/turbojet-package/turbojet-1.0.0}"
mode="both"
if [[ $# -gt 0 && "$1" != -* && -d "$1" ]]; then package="$1"; shift; fi
if [[ $# -gt 1 ]]; then echo "usage: $0 [PACKAGE_DIR] [empty|shipped|synthetic|both|all]" >&2; exit 2; fi
if [[ $# -eq 1 ]]; then mode="$1"; fi
case "$mode" in empty|shipped|synthetic|both|all) ;; *) echo "invalid mode: $mode" >&2; exit 2 ;; esac
[[ -d "$package" ]] || { echo "package not found: $package" >&2; exit 2; }

log_dir="${TJ_PACKAGE_SMOKE_LOG_DIR:-/tmp/opencode}"
mkdir -p "$log_dir" 2>/dev/null || log_dir="$(mktemp -d)"
state="$(mktemp -d "${TMPDIR:-/tmp}/turbojet-package-smoke.XXXXXX")"
trap 'rm -rf "$state"' EXIT

python3 - "$state" "$package" "$mode" "$log_dir" <<'PY'
import json, os, shutil, signal, socket, subprocess, sys, time, urllib.error, urllib.request
from pathlib import Path

state = Path(sys.argv[1])
package = Path(sys.argv[2])
requested = sys.argv[3]
log_dir = Path(sys.argv[4])

def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]

def http(base, path, method="GET", token=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(base + path, data=data, method=method)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=3) as response:
            raw = response.read()
            try:
                return response.status, json.loads(raw)
            except json.JSONDecodeError:
                return response.status, raw.decode(errors="replace")
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, raw.decode(errors="replace")

def strip_shipped_assets(app):
    config = app / "config"
    for path in config.glob("*.smg.json"):
        path.unlink()
    for directory in [config / "local", app / "examples", app / "workflows"]:
        shutil.rmtree(directory, ignore_errors=True)

def write_synthetic(app):
    config_dir = app / "config"
    package_dir = config_dir / "synthetic-package"
    agent_dir = package_dir / "agents" / "smoke"
    agent_dir.mkdir(parents=True)
    (package_dir / "schemas").mkdir(parents=True)
    graph = {
        "version": 1,
        "name": "Package smoke synthetic",
        "maturity": "experimental",
        "description": "Exact synthetic package injected into a disposable package copy",
        "catalog": {"id": "smoke-synthetic", "label": "Smoke synthetic"},
        "entry": "flow",
        "exit": "flow",
        "limits": {"maxIterations": 1, "maxSteps": 5, "maxDepth": 2},
        "flow-order": ["flow"],
        "flows": {
            "flow": {
                "type": "flow",
                "entry": "step",
                "exit": "step",
                "stages": {
                    "step": {
                        "agent": "smoke",
                        "interactive": False,
                        "routes": {"success": "_exit"},
                    }
                },
            }
        },
    }
    (package_dir / "workflow.smg.json").write_text(json.dumps(graph, indent=2) + "\n")
    (agent_dir / "agent.json").write_text(json.dumps({
        "id": "smoke",
        "name": "Smoke",
        "prompt": "prompt.md",
        "schema": "schemas/smoke.json",
        "timeout": 5,
        "max_output_retries": 0,
        "tools": {"read": False, "write": False, "edit": False, "bash": False},
    }, indent=2) + "\n")
    (agent_dir / "prompt.md").write_text("package smoke fixture\n")
    (package_dir / "schemas" / "smoke.json").write_text(json.dumps({
        "type": "object",
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string", "enum": ["success"]}},
    }, indent=2) + "\n")
    return package_dir

def edit_config(app, port, mode, synthetic_source=None):
    config = app / "config" / "turbojet.selfhosted.json"
    if not config.is_file():
        config = app / "config" / "turbojet.json"
    value = json.loads(config.read_text())
    value["listen_addr"] = f"127.0.0.1:{port}"
    value["database_path"] = "$TURBOJET_HOME/state.db"
    value["workspace_base"] = "$TURBOJET_HOME/workspaces"
    value["log_level"] = "warn"
    value.pop("smg_path", None)
    if mode == "empty":
        value["workflow_sources"] = []
    elif mode == "shipped":
        value["workflow_sources"] = [{
            "id": "examples",
            "path": "workflows",
            "kind": "directory",
        }]
    else:
        value["workflow_sources"] = [{
            "id": "package-smoke-synthetic",
            "path": str(synthetic_source.relative_to(app)),
            "kind": "package",
        }]
    config.write_text(json.dumps(value, indent=2) + "\n")
    return config

def run_mode(mode):
    app = state / f"app-{mode}"
    shutil.copytree(package, app)
    if mode != "shipped":
        strip_shipped_assets(app)
    else:
        assert (app / "workflows/discovery/workflow.smg.json").is_file(), \
            "shipped: package does not contain workflows/discovery"
        assert not (app / "config/local").exists(), \
            "shipped: package must not contain config/local"
    synthetic = write_synthetic(app) if mode == "synthetic" else None
    port = free_port()
    config = edit_config(app, port, mode, synthetic)
    home = state / f"home-{mode}"
    home.mkdir()
    env = os.environ.copy()
    env.update({
        "TURBOJET_HOME": str(home),
        "TURBOJET_WEBUI_PATH": str(app / "share/webui/index.html"),
        "TURBOJET_MASTER_KEY": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "TURBOJET_PROVIDER_ZAI_API_KEY": "local-package-smoke",
        "TZ": "UTC",
    })
    log_path = log_dir / f"package-smoke-{mode}.log"
    log = log_path.open("w")
    process = subprocess.Popen([str(app / "bin/tj-api"), str(config)], env=env,
                               stdout=log, stderr=subprocess.STDOUT)
    base = f"http://127.0.0.1:{port}"
    try:
        for _ in range(150):
            if process.poll() is not None:
                raise SystemExit(f"{mode}: server exited early; see {log_path}")
            try:
                status, _ = http(base, "/api/health")
                if status == 200:
                    break
            except Exception:
                time.sleep(0.1)
        else:
            raise SystemExit(f"{mode}: server did not become healthy; see {log_path}")

        status, html = http(base, "/")
        assert status == 200 and "<html" in html.lower(), f"{mode}: packaged UI not served"
        assert "Chief" in html, f"{mode}: packaged UI does not contain the canonical Chief label"
        assert "No workflow installed" in html, f"{mode}: packaged UI lacks empty-registry guidance"
        status, ready = http(base, "/api/ready")
        assert status == 200, f"{mode}: readiness status {status}: {ready}"
        checks = {check["name"]: check for check in ready.get("checks", [])}
        workflows_check = checks["workflows"]
        status, auth = http(base, "/v1/auth/register", "POST", body={
            "username": "package-smoke", "password": "package-smoke-pass"})
        assert status == 200, f"{mode}: register {status}: {auth}"
        token = auth["token"]
        # First-run guidance advances past administrator creation only after the
        # real registration; the workflow step must be the next actionable item.
        status, ready = http(base, "/api/ready")
        assert status == 200, f"{mode}: readiness after register {status}: {ready}"
        checks = {check["name"]: check for check in ready.get("checks", [])}
        workflows_check = checks["workflows"]
        status, profiles = http(base, "/v1/profiles", token=token)
        assert status == 200, f"{mode}: profiles {status}: {profiles}"
        status, workflows = http(base, "/v1/workflows", token=token)
        assert status == 200, f"{mode}: workflows {status}: {workflows}"

        if mode == "empty":
            assert workflows_check["ok"] is False, workflows_check
            assert workflows_check["count"] == 0, workflows_check
            assert profiles.get("profiles") == [], profiles
            assert workflows.get("workflows") == [], workflows
            assert workflows.get("configured") is None, workflows
            assert "No workflow installed" in ready["first_run"]["next_step"], ready["first_run"]
            status, project = http(base, "/v1/projects", "POST", token, {
                "name": "must-not-exist"})
            assert status == 409, f"empty: project without workflow must be 409, got {status}: {project}"
            assert "workflow" in json.dumps(project).lower(), project
        else:
            assert workflows_check["ok"] is True, workflows_check
            assert workflows_check["count"] >= 1, workflows_check
            entries = profiles.get("profiles", [])
            versions = workflows.get("workflows", [])
            if mode == "shipped":
                expected = {"discovery", "agent-loop", "clarify-produce-review",
                            "research-synthesize-verify"}
                catalog_ids = {entry.get("catalog_id") for entry in entries}
                assert expected <= catalog_ids, (expected, catalog_ids)
                for version in versions:
                    if version.get("catalog_id") in expected:
                        assert version.get("source_kind") == "package", version
                        assert version.get("asset_count", 0) > 0, version
                catalog_id = "discovery"
                project_name = "shipped package project"
            else:
                assert any(entry.get("catalog_id") == "smoke-synthetic" for entry in entries), entries
                catalog_id = "smoke-synthetic"
                project_name = "synthetic package project"
            version = next(v for v in versions if v.get("catalog_id") == catalog_id)
            assert version.get("capability_schema_version") == 1, version
            status, project = http(base, "/v1/projects", "POST", token, {
                "name": project_name, "catalog_id": catalog_id})
            assert status == 201, f"{mode}: project {status}: {project}"
            lane = project.get("lane") or {}
            assert lane.get("workflow_id") == version.get("id"), (lane, version)

        print(f"package smoke {mode}: passed (port {port}, readiness {ready['status']}, "
              f"workflows {workflows_check['count']}); log {log_path}")
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        log.close()

if requested == "both":
    modes = ["empty", "synthetic"]
elif requested == "all":
    modes = ["empty", "shipped", "synthetic"]
else:
    modes = [requested]
for item in modes:
    run_mode(item)
PY
