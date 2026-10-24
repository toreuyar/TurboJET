#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export TURBOJET_WEBUI_PATH="${TURBOJET_WEBUI_PATH:-$REPO_ROOT/webui/dist/index.html}"
exec "$REPO_ROOT/start.sh" "$@"
