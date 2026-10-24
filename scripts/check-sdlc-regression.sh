#!/usr/bin/env bash
# Explicit local full-SDLC operational regression. This is never part of the
# default asset-free suite. It proves the ignored derived package executes
# through the real API -> Manager -> Flow -> Stage -> publication -> delivery
# path using a scripted local model, a disposable bare Git repository and a
# local HTTP Forgejo-protocol fixture.
#
# Usage:
#   bash scripts/check-sdlc-regression.sh
#   TJ_SDLC_PACKAGE=/path/to/full-sdlc.package bash scripts/check-sdlc-regression.sh
#
# Absence of the local asset is reported as BLOCKED (exit 2), never a false
# success. No live provider/forge/CI/deployment is contacted.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
original="$root/config/local/full-sdlc.smg.json"
package="${TJ_SDLC_PACKAGE:-$root/config/local/full-sdlc.package}"
expected_hash="a88bed13195f925f9af01169ae9b438e3e161cdbc05257443569a25530f5d34f"
log_dir="${TJ_SDLC_LOG_DIR:-/tmp/opencode}"
mkdir -p "$log_dir" 2>/dev/null || log_dir="$(mktemp -d)"
log="$log_dir/sdlc-regression.log"

if [[ ! -f "$original" ]]; then
  echo "BLOCKED: original local SDLC graph is absent: $original" >&2
  exit 2
fi
if [[ ! -f "$package/workflow.smg.json" ]]; then
  echo "BLOCKED: derived local SDLC package is absent: $package" >&2
  echo "Run: bash scripts/import-local-sdlc.sh derive" >&2
  exit 2
fi
actual="$(sha256sum "$original" | awk '{print $1}')"
if [[ "$actual" != "$expected_hash" ]]; then
  echo "FAILED: original local SDLC hash changed: $actual != $expected_hash" >&2
  exit 1
fi

export TJ_SDLC_PACKAGE="$package"
if [[ -n "${TJ_SDLC_CARGO_HOME:-}" ]]; then
  export CARGO_HOME="$TJ_SDLC_CARGO_HOME"
elif [[ -d "$HOME/.cargo/git" ]]; then
  export CARGO_HOME="$HOME/.cargo"
else
  export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
fi
offline=()
if [[ "${TJ_OFFLINE:-0}" == 1 ]]; then offline=(--offline); fi

echo "== local SDLC package shape/derivation check =="
cargo test -p tj-config --features example-workflows --locked --test smg_route_schemas_test \
  "${offline[@]}" derived_local_sdlc_package_preserves_the_original_graph \
  2>&1 | tee "$log"

echo "== local SDLC operational API/Manager/Flow/Stage/publication/delivery regression =="
cargo test -p tj-api --features example-workflows --locked --test engineering_harness \
  "${offline[@]}" local_derived_sdlc_package_runs_declared_capabilities_end_to_end -- --nocapture \
  2>&1 | tee -a "$log"

actual_after="$(sha256sum "$original" | awk '{print $1}')"
if [[ "$actual_after" != "$expected_hash" ]]; then
  echo "FAILED: original local SDLC hash changed during regression: $actual_after" >&2
  exit 1
fi
echo "local SDLC regression passed; original hash unchanged: $actual_after; log: $log"
