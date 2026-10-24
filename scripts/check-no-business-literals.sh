#!/usr/bin/env bash
# Guard the default test layer against shipped workflow/business contracts and
# repository asset reads. This intentionally scans only test code; production
# workflow-name branches are removed by Batch 4 and are not rewritten here.
#
# Allowlist (explicit, reviewable):
#   - optional Rust example/distribution suites behind `example-workflows`
#   - optional web example suite and non-default browser journeys
#   - the future legacy v1 compatibility adapter (inline frozen bundle)
# Historical docs and migration adapters are not default engine tests.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])

RUST_ALLOW = {
    "crates/tj-config/tests/agent_schemas_test.rs",
    "crates/tj-config/tests/config_lint.rs",
    "crates/tj-config/tests/generic_workflows_test.rs",
    "crates/tj-config/tests/smg_route_schemas_test.rs",
    "crates/tj-api/tests/delivery_harness.rs",
    "crates/tj-api/tests/discovery_packet_harness.rs",
    "crates/tj-api/tests/engineering_harness.rs",
    "crates/tj-api/tests/generic_workflows_harness.rs",
    "crates/tj-api/tests/planning_harness.rs",
    "crates/tj-manager/tests/legacy_v1_adapter.rs",
}
WEB_ALLOW_PREFIXES = (
    "webui/tests/examples/",
    "webui/tests/e2e/backend-",
    "webui/tests/e2e/live-",
    "webui/tests/e2e/planning-journey.spec.ts",
)

BUSINESS = re.compile(
    r"\b(?:discovery|agent-loop|clarify-produce-review|research-synthesize-verify|full-sdlc|default\.smg)\b",
    re.IGNORECASE,
)
ASSET_REF = re.compile(
    r"config/turbojet\.json|config/[A-Za-z0-9_./{}$-]*\.smg\.json|config/(?:agents|schemas|local)/|examples/|include_str!\([^)]*config"
)

def rust_test_blocks(text: str) -> list[tuple[int, str]]:
    """Return (1-based start line, text) for inline #[cfg(test)] modules."""
    blocks: list[tuple[int, str]] = []
    for match in re.finditer(r"#\[cfg\(test\)\]", text):
        brace = text.find("{", match.end())
        if brace < 0:
            continue
        depth = 0
        end = None
        for index in range(brace, len(text)):
            char = text[index]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    end = index + 1
                    break
        if end is None:
            continue
        start_line = text.count("\n", 0, match.start()) + 1
        blocks.append((start_line, text[match.start():end]))
    return blocks

def collect_rust() -> list[tuple[str, int, str]]:
    sources: list[tuple[str, int, str]] = []
    for path in sorted((root / "crates").glob("*/**/*.rs")):
        rel = path.relative_to(root).as_posix()
        if "/target/" in rel:
            continue
        is_test_file = "/tests/" in rel or path.name == "tests.rs" or path.name.endswith("_tests.rs")
        if is_test_file:
            if rel in RUST_ALLOW:
                continue
            sources.append((rel, 1, path.read_text()))
        else:
            for start, block in rust_test_blocks(path.read_text()):
                sources.append((rel, start, block))
    return sources

def collect_web() -> list[tuple[str, int, str]]:
    sources: list[tuple[str, int, str]] = []
    for directory in ("webui/tests/unit", "webui/tests/e2e"):
        for path in sorted((root / directory).glob("*")):
            if not path.is_file() or path.suffix not in {".ts", ".tsx"}:
                continue
            rel = path.relative_to(root).as_posix()
            if rel.startswith(WEB_ALLOW_PREFIXES):
                continue
            if directory.endswith("e2e") and path.name != "webui.spec.ts":
                # Optional backend/live browser suites are explicit jobs.
                continue
            sources.append((rel, 1, path.read_text()))
    return sources

violations: list[str] = []
for rel, base_line, text in collect_rust() + collect_web():
    for offset, line in enumerate(text.splitlines()):
        if BUSINESS.search(line) or ASSET_REF.search(line):
            violations.append(f"{rel}:{base_line + offset}: {line.strip()}")

# Production behavior must not select a flow/stage/gate/profile/catalog by a
# business-name literal. Historical names are confined to the explicit legacy
# adapter and its policy table; comments/messages alone are not behavior.
BEHAVIOR_LITERAL = (
    r"(?:discovery|planning|engineering|agent-loop|clarify-produce-review|"
    r"research-synthesize-verify|full-sdlc|test|review|acceptance)"
)
BEHAVIOR = re.compile(
    rf'(?:(?:flow_name|flow|gate|stage|profile|catalog_id|workflow)\s*==\s*"{BEHAVIOR_LITERAL}"'
    rf'|"{BEHAVIOR_LITERAL}"\s*==\s*(?:flow_name|flow|gate|stage|profile|catalog_id|workflow))'
)
PRODUCTION_ALLOW = {
    "crates/tj-config/src/profiles.rs",
    "crates/tj-manager/src/legacy_v1.rs",
}
for path in sorted((root / "crates").glob("*/src/**/*.rs")):
    rel = path.relative_to(root).as_posix()
    if rel in PRODUCTION_ALLOW or "/target/" in rel:
        continue
    text = path.read_text()
    # Inline test modules are covered by the default-test scan above; do not
    # let their synthetic fixtures look like production inference.
    production = text.split("#[cfg(test)]", 1)[0]
    for offset, line in enumerate(production.splitlines(), 1):
        if BEHAVIOR.search(line):
            violations.append(f"{rel}:{offset}: {line.strip()}")

if violations:
    print("Default test layer references shipped workflow/business assets:", file=sys.stderr)
    for violation in violations:
        print(f"  {violation}", file=sys.stderr)
    print(
        "\nMove the contract to the optional example/domain suite or use tj-test-support synthetic fixtures.",
        file=sys.stderr,
    )
    sys.exit(1)

print("check-no-business-literals: default test layer and production selectors are synthetic/asset-free")
PY
