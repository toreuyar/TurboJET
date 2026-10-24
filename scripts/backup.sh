#!/usr/bin/env bash
# Supported backup policy: stop the service -> run this offline backup -> start.
# tj-ops takes the same exclusive <state.db>.lock as a live server and refuses a
# hot backup, so a running service fails this wrapper explicitly.
# Requires TURBOJET_MASTER_KEY when encrypted secrets exist.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
db="${TJ_DB_PATH:-${TURBOJET_HOME:-$HOME/.turbojet}/state.db}"
artifacts="${TJ_ARTIFACTS_PATH:-${TURBOJET_HOME:-$HOME/.turbojet}/workspaces}"
out="${1:-${TURBOJET_HOME:-$HOME/.turbojet}/backups/$(date -u +%Y%m%dT%H%M%SZ)}"
binary="${TJ_OPS_BINARY:-${CARGO_TARGET_DIR:-$root/target}/debug/tj-ops}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
if [[ ! -x "$binary" ]]; then
  (cd "$root" && cargo build -p tj-ops)
fi
"$binary" backup --db "$db" --artifacts "$artifacts" --out "$out"
echo "backup at $out (verify with: $binary verify --backup '$out')" >&2
