# Test Teardown — Read and Execute After Every Test Run

Mandatory. Execute regardless of test results (pass, fail, or crash). Do not skip.

## 1. Cancel Active Runs

```bash
RUNS=$(curl -sf http://localhost:8100/v1/pipeline/runs 2>/dev/null | jq -r '.[] | select(.status == "running" or .status == "pending") | .id' 2>/dev/null)
if [ -n "$RUNS" ]; then
  for id in $RUNS; do
    curl -sf -X POST "http://localhost:8100/v1/pipeline/runs/$id/cancel" > /dev/null 2>&1
    echo "Cancelled: $id"
  done
else
  echo "No active runs to cancel"
fi
```

## 2. Clean Test Repositories

Only remove paths created by the test scenario:
```bash
rm -rf /tmp/test-repo /tmp/turbojet-test-* 2>/dev/null
echo "Test repos cleaned"
```

## 3. Stop Server

```bash
./scripts/stop_server.sh
```

## 4. Report Teardown

```
TEARDOWN COMPLETE
Runs cancelled: [count]
Server stopped: [yes/no]
Residual data: [none/describe]
```

## Constraints

- Do NOT read server logs during teardown.
- Do NOT investigate why the server crashed (if it did). Report the fact, that's all.
- QA manages the full server lifecycle. Always stop the server in teardown.
