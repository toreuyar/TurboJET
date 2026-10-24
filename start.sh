#!/usr/bin/env bash
set -euo pipefail

RUST_BACKTRACE=1
TURBOJET_HOME="${TURBOJET_HOME:-$HOME/.turbojet}"
mkdir -p "$TURBOJET_HOME"

export RUST_BACKTRACE TURBOJET_HOME

CONFIG="${1:-config/turbojet.json}"
cd "$(dirname "${BASH_SOURCE[0]}")"

if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
    if [ -z "${CARGO_HOME:-}" ] || [ ! -w "$CARGO_HOME" ]; then
        export CARGO_HOME="$HOME/.cargo"
    fi
fi

if [ -f "config/.env" ]; then
    set -a
    source config/.env
    set +a
fi

echo "Building TurboJet..."
cargo build 2>&1 || { echo "BUILD FAILED"; exit 1; }
echo "Build complete. Starting TurboJet..."
exec target/debug/tj-api "$CONFIG"
