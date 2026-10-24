#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export TURBOJET_HOME="${TURBOJET_TEST_HOME:-$HOME/.turbojet-test}"
PID_FILE="$TURBOJET_HOME/server.pid"
LOG_FILE="$TURBOJET_HOME/server.log"

load_env() {
  if [[ -f "$REPO_ROOT/config/.env" ]]; then
    set -a; source "$REPO_ROOT/config/.env"; set +a
  fi
}

cmd_status() {
  if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "running (pid $(cat "$PID_FILE"))"
    curl -sf -m 3 "http://127.0.0.1:${TURBOJET_TEST_PORT:-8100}/api/health" && echo
  else
    echo "stopped"
    exit 1
  fi
}

cmd_start() {
  if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "already running (pid $(cat "$PID_FILE"))"
    exit 0
  fi
  if [[ ! -x "$REPO_ROOT/target/debug/tj-api" ]]; then
    echo "building tj-api…"
    (cd "$REPO_ROOT" && CARGO_HOME="$HOME/.cargo" "$HOME/.cargo/bin/cargo" build -p tj-api)
  fi
  load_env
  export TURBOJET_WEBUI_PATH="${TURBOJET_WEBUI_PATH:-$REPO_ROOT/webui/dist/index.html}"
  cd "$REPO_ROOT"
  nohup setsid "$REPO_ROOT/target/debug/tj-api" "$TURBOJET_HOME/config.json" >>"$LOG_FILE" 2>&1 &
  echo $! > "$PID_FILE"
  sleep 2
  cmd_status
}

cmd_stop() {
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid="$(cat "$PID_FILE")"
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid"
      sleep 1
      kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
      echo "stopped (pid $pid)"
    else
      echo "not running (stale pid file)"
    fi
    rm -f "$PID_FILE"
  else
    echo "not running"
  fi
}

cmd_logs() {
  [[ -f "$LOG_FILE" ]] && tail -f "$LOG_FILE" || echo "no log yet at $LOG_FILE"
}

cmd_reset() {
  cmd_stop >/dev/null 2>&1 || true
  rm -f "$TURBOJET_HOME/state.db" "$TURBOJET_HOME/state.db-wal" "$TURBOJET_HOME/state.db-shm"
  rm -rf "$TURBOJET_HOME/workspace"/*
  echo "test state reset (db cleared, workspace emptied)"
}

case "${1:-status}" in
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  status) cmd_status ;;
  logs)   cmd_logs ;;
  reset)  cmd_reset ;;
  *) echo "usage: $0 {start|stop|status|logs|reset}" >&2; exit 2 ;;
esac
