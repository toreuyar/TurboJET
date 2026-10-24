#!/usr/bin/env bash
# Build a local, versioned TurboJet package. Never installs or publishes.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="${TJ_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)}"
out="${TJ_PACKAGE_OUT:-/tmp/opencode/turbojet-package}"
binary="${TJ_API_BINARY:-$root/target/debug/tj-api}"
webui="${TJ_WEBUI_PATH:-$root/webui/dist/index.html}"
ops_binary="${TJ_OPS_BINARY:-${CARGO_TARGET_DIR:-$root/target}/debug/tj-ops}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

if [[ ! -f "$webui" ]]; then
  echo "building webui…" >&2
  (cd "$root/webui" && npm run build)
fi
if [[ ! -x "$binary" ]]; then
  echo "building tj-api…" >&2
  (cd "$root" && cargo build -p tj-api -p tj-ops)
fi
if [[ ! -x "$ops_binary" ]]; then
  (cd "$root" && cargo build -p tj-ops)
fi

mkdir -p "$out"
"$ops_binary" package \
  --out "$out" \
  --binary "$binary" \
  --webui "$webui" \
  --config "$root/config" \
  --workflows "$root/workflows" \
  --version "$version"

echo "local package at $out/turbojet-$version (not installed, not published)" >&2
