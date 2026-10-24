#!/usr/bin/env bash
# Prove the default Rust/web suites do not need shipped workflow assets.
#
# The real checkout is never modified. A disposable copy is made, shipped
# graph/package/agent/schema/local/example assets are removed *in the copy
# only*, then the default suites run. Use --keep to inspect the copy and logs.
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
keep=0
for arg in "$@"; do
  case "$arg" in
    --keep) keep=1 ;;
    *) echo "Usage: bash scripts/check-asset-free.sh [--keep]" >&2; exit 2 ;;
  esac
done

# Offline is the default proof. A caller may opt into an online run explicitly,
# but CI must leave this at 1 so the disposable suite cannot fetch late.
offline="${TJ_ASSET_FREE_OFFLINE:-1}"
case "$offline" in
  0|1) ;;
  *) echo "TJ_ASSET_FREE_OFFLINE must be 0 or 1" >&2; exit 2 ;;
esac

log_dir="${TJ_ASSET_FREE_LOG_DIR:-/tmp/opencode}"
mkdir -p "$log_dir" 2>/dev/null || log_dir="$(mktemp -d)"
work="$(mktemp -d "${TMPDIR:-/tmp}/turbojet-asset-free.XXXXXX")"
copy="$work/src"
cleanup() {
  if [[ "$keep" == 1 ]]; then
    echo "asset-free copy kept at: $copy" >&2
    echo "asset-free logs kept at: $log_dir" >&2
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT

if [[ "$copy" == "$root" || "$root" == "$copy"* ]]; then
  echo "refusing to run in the real checkout" >&2
  exit 2
fi
if ! command -v rsync >/dev/null 2>&1; then
  echo "rsync is required for the disposable copy" >&2
  exit 2
fi

# 1. Disposable source copy: no VCS, build outputs, runtime state, local
#    OpenCode/isolator dirs or node modules.
rsync -a \
  --exclude '.git/' --exclude 'target/' --exclude 'check-results/' \
  --exclude 'webui/node_modules/' --exclude 'webui/test-results/' \
  --exclude 'webui/playwright-report/' --exclude 'webui/dist/' \
  --exclude '.factory/' --exclude '.isolator-staging/' --exclude '.opencode/' \
  --exclude '.turbojet/' --exclude 'opencode.json' \
  --exclude '.env' --exclude '.env.*' \
  "$root/" "$copy/"

# 2. Remove shipped assets in the copy only. The user's checkout and
#    config/local/full-sdlc.smg.json are untouched. Shipped workflow packages
#    under workflows/ are distribution assets too, as are the optional
#    test-package fixtures. Default tests must remain synthetic.
find "$copy/config" -maxdepth 1 -type f -name '*.smg.json' -delete
rm -rf "$copy/config/local" "$copy/examples" "$copy/workflows" "$copy/tests/fixtures/packages"

# Prove the copy really is asset-free before spending time on the suite. A
# failed removal must abort instead of silently testing a checkout that still
# contains the distribution assets.
leftover_smgs="$(find "$copy/config" -maxdepth 1 -type f -name '*.smg.json' -print -quit)"
if [[ -n "$leftover_smgs" ]]; then
  echo "asset-free copy still contains a workflow graph: $leftover_smgs" >&2
  exit 1
fi
for path in config/local examples workflows tests/fixtures/packages; do
  if [[ -e "$copy/$path" ]]; then
    echo "asset-free copy still contains $path" >&2
    exit 1
  fi
done

# 3. Reuse installed web dependencies read-only if possible. If absent, the
#    copy still has package-lock.json and can install its own node_modules.
if [[ -d "$root/webui/node_modules" ]]; then
  ln -s "$root/webui/node_modules" "$copy/webui/node_modules"
fi

export TURBOJET_HOME="$work/state"
export TURBOJET_WORKSPACE="$work/workspace"
export TMPDIR="$work/tmp"
mkdir -p "$TURBOJET_HOME" "$TURBOJET_WORKSPACE" "$TMPDIR"
# Use the local cache but a disposable target directory. Offline keeps the
# proof deterministic and prevents an unnoticed network dependency. Some
# harnesses export a read-only CARGO_HOME; prefer a populated user cache so git
# dependencies remain available offline. CI passes its fetched cache through
# TJ_ASSET_FREE_CARGO_HOME explicitly.
if [[ -n "${TJ_ASSET_FREE_CARGO_HOME:-}" ]]; then
  export CARGO_HOME="$TJ_ASSET_FREE_CARGO_HOME"
elif [[ -d "$HOME/.cargo/git" || -d "$HOME/.cargo/registry" ]]; then
  export CARGO_HOME="$HOME/.cargo"
else
  export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
fi
export CARGO_TARGET_DIR="$work/target"

rust_log="$log_dir/asset-free-rust.log"
web_log="$log_dir/asset-free-web.log"
guard_log="$log_dir/asset-free-guard.log"
fetch_log="$log_dir/asset-free-fetch.log"

rust_status=0
web_status=0

if [[ "$offline" == 1 ]]; then
  echo "== cargo fetch --locked --offline (locked dependency preflight) =="
  if ! (
    cd "$copy"
    cargo fetch --locked --offline
  ) >"$fetch_log" 2>&1; then
    cat "$fetch_log" >&2
    echo "asset-free bootstrap failed: locked dependencies are not cached in CARGO_HOME=$CARGO_HOME." >&2
    echo "Run 'cargo fetch --locked' with that CARGO_HOME before this offline check." >&2
    exit 1
  fi
fi

echo "== check-no-business-literals (disposable copy) =="
if ! bash "$copy/scripts/check-no-business-literals.sh" >"$guard_log" 2>&1; then
  rust_status=1
  cat "$guard_log" >&2
fi

cargo_test=(test --workspace --locked)
if [[ "$offline" == 1 ]]; then
  cargo_test+=(--offline)
fi
echo "== cargo ${cargo_test[*]} (no shipped assets) =="
(
  cd "$copy"
  cargo "${cargo_test[@]}"
) >"$rust_log" 2>&1 || rust_status=1

echo "== npm test (no shipped assets) =="
(
  cd "$copy/webui"
  if [[ ! -d node_modules ]]; then
    npm ci --no-audit --no-fund
  fi
  npm test
) >"$web_log" 2>&1 || web_status=1

if [[ "$rust_status" != 0 ]]; then
  echo "Rust asset-free suite FAILED; tail of $rust_log:" >&2
  tail -60 "$rust_log" >&2
fi
if [[ "$web_status" != 0 ]]; then
  echo "Web asset-free suite FAILED; tail of $web_log:" >&2
  tail -60 "$web_log" >&2
fi
if [[ "$rust_status" == 0 && "$web_status" == 0 ]]; then
  echo "asset-free proof passed (rust + web); logs: $rust_log $web_log"
  exit 0
fi
exit 1
