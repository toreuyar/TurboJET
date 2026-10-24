#!/usr/bin/env bash
# Mock OpenAI-compatible LLM for TurboJet test runs (port 9101).
# Streams chat/completions responses ending with the ::TURBOJET:DONE:: marker
# so interactive stages complete in one turn without a chat session.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG=/tmp/opencode/mock_llm.log

case "${1:-start}" in
  start)
    if curl -sf -m 2 http://127.0.0.1:9101/v1/models >/dev/null 2>&1; then echo "mock-llm already running"; exit 0; fi
    setsid nohup python3 "$SCRIPT_DIR/mock_llm.py" >"$LOG" 2>&1 < /dev/null &
    sleep 1
    if curl -s -m 2 http://127.0.0.1:9101/v1/chat/completions -d '{"stream":true,"messages":[{"role":"user","content":"ping"}]}' | grep -q "DONE"; then
      echo "mock-llm running on :9101"
    else
      echo "mock-llm failed to start"; cat "$LOG"; exit 1
    fi
    ;;
  stop) pkill -f "mock_llm.py" 2>/dev/null && echo "mock-llm stopped" || echo "mock-llm not running" ;;
  status) curl -sf -m 2 http://127.0.0.1:9101/v1/chat/completions -d '{"stream":true,"messages":[{"role":"user","content":"ping"}]}' | grep -q DONE && echo "running" || echo "stopped" ;;
esac