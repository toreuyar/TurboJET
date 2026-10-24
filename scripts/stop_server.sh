#!/usr/bin/env bash
set -euo pipefail

killed_pids=()

# --- Graceful shutdown: SIGTERM first, wait, then SIGKILL ---
for pattern in "tj-api" "start.sh"; do
    pids=$(pgrep -f "$pattern" 2>/dev/null || true)
    if [[ -n "$pids" ]]; then
        for pid in $pids; do
            # Send SIGTERM for graceful shutdown
            kill -TERM "$pid" 2>/dev/null && killed_pids+=("$pid (SIGTERM)") || true
        done
    fi
done

# Wait up to 15 seconds for graceful shutdown
if [[ ${#killed_pids[@]} -gt 0 ]]; then
    echo "Sent SIGTERM, waiting up to 15s for graceful shutdown..."
    elapsed=0
    while [ "$elapsed" -lt 15 ]; do
        remaining=$(pgrep -f "tj-api|start.sh" 2>/dev/null || true)
        [[ -z "$remaining" ]] && break
        sleep 1
        elapsed=$((elapsed + 1))
    done
fi

# Escalate to SIGKILL if still alive
remaining=$(pgrep -f "tj-api|start.sh" 2>/dev/null || true)
if [[ -n "$remaining" ]]; then
    echo "Processes still alive, escalating to SIGKILL..."
    for pid in $remaining; do
        pgid=$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ' || true)
        if [[ -n "$pgid" ]]; then
            sudo kill -9 -- -"$pgid" 2>/dev/null && killed_pids+=("$pid (pgid $pgid SIGKILL)") || true
        fi
        sudo kill -9 "$pid" 2>/dev/null && killed_pids+=("$pid (SIGKILL)") || true
    done
fi

# Deduplicate killed_pids
if [[ ${#killed_pids[@]} -gt 0 ]]; then
    mapfile -t killed_pids < <(printf '%s\n' "${killed_pids[@]}" | sort -u)
fi

# --- Verify everything is dead ---
remaining_tj=$(pgrep -f "tj-api" 2>/dev/null || true)
remaining_start=$(pgrep -f "start.sh" 2>/dev/null || true)

if [[ -n "$remaining_tj" || -n "$remaining_start" ]]; then
    echo "FAILURE: processes still alive after kill attempt"
    [[ -n "$remaining_tj" ]] && echo "  tj-api PIDs still running: $remaining_tj"
    [[ -n "$remaining_start" ]] && echo "  start.sh PIDs still running: $remaining_start"
    exit 1
fi

# --- Report ---
if [[ ${#killed_pids[@]} -eq 0 ]]; then
    echo "No tj-api or start.sh processes found."
else
    echo "Killed ${#killed_pids[@]} process(es):"
    for p in "${killed_pids[@]}"; do
        echo "  - $p"
    done
fi

echo "All TurboJet server processes stopped."
