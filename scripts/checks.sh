#!/usr/bin/env bash
# Deterministic checks only. Live/provider checks are explicit separate commands.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
mode="${1:-all}"
case "$mode" in all|rust|web|examples|asset-free) ;; *) echo 'Usage: bash scripts/checks.sh [all|rust|web|examples|asset-free]' >&2; exit 2 ;; esac
if [[ "$mode" == asset-free ]]; then exec bash "$root/scripts/check-asset-free.sh" "${@:2}"; fi
results="${TJ_CHECK_RESULTS:-$root/check-results}"
mkdir -p "$results"
results="$(cd "$results" && pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/turbojet-checks.XXXXXX")"
trap 'rm -rf "$state"' EXIT
export TURBOJET_HOME="$state/state"
export TURBOJET_WORKSPACE="$state/workspace"
export TMPDIR="$state/tmp"
mkdir -p "$TURBOJET_HOME" "$TURBOJET_WORKSPACE" "$TMPDIR"

if [[ "$mode" != web && "$mode" != examples ]]; then
  bash "$root/scripts/check-no-business-literals.sh" 2>&1 | tee "$results/no-business-literals.log"
  cargo_args=(test --workspace --locked)
  if [[ "${TJ_OFFLINE:-0}" == 1 ]]; then cargo_args+=(--offline); fi
  cargo "${cargo_args[@]}" 2>&1 | tee "$results/rust.log"
fi
if [[ "$mode" == examples ]]; then
  status=0
  example_offline=()
  if [[ "${TJ_OFFLINE:-0}" == 1 ]]; then example_offline=(--offline); fi
  cargo test -p tj-config -p tj-api --features example-workflows --locked --no-fail-fast "${example_offline[@]}" 2>&1 | tee "$results/rust-examples.log" || status=1
  cargo test -p tj-manager --features example-workflows --locked --no-fail-fast "${example_offline[@]}" 2>&1 | tee "$results/rust-manager-examples.log" || status=1
  cd "$root/webui"
  npm run test:examples 2>&1 | tee "$results/web-examples.log" || status=1
  exit "$status"
fi
if [[ "$mode" != rust ]]; then
  cd "$root/webui"
  npm ci --no-audit --no-fund 2>&1 | tee "$results/npm-ci.log"
  if [[ "${TJ_INSTALL_BROWSER:-0}" == 1 ]]; then
    npx playwright install --with-deps chromium 2>&1 | tee "$results/browser-install.log"
  fi
  npm test 2>&1 | tee "$results/web-unit.log"
  # e2e builds production assets first, then checks Chromium prerequisites and
  # runs only mocked cases against a newly owned preview server.
  npm run e2e 2>&1 | tee "$results/web-e2e.log"
  if [[ "${TJ_RUN_BACKEND_E2E:-0}" == 1 ]]; then
    # Real API/Manager/Flow/Stage + built UI + local scripted model. Disposable
    # state only; never a live provider.
    backend_offline=()
    if [[ "${TJ_OFFLINE:-0}" == 1 ]]; then backend_offline=(--offline); fi
    cargo build --manifest-path "$root/Cargo.toml" -p tj-api "${backend_offline[@]}" 2>&1 | tee "$results/backend-build.log"
    TJ_API_BINARY="${TJ_API_BINARY:-${CARGO_TARGET_DIR:-$root/target}/debug/tj-api}" \
      npx playwright test --config playwright.backend.config.ts 2>&1 | tee "$results/web-e2e-backend.log"
  fi
fi
