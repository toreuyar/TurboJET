# Test Setup — Read and Execute Before Every Test Run

Mandatory steps. Do not skip any step. Execute in order.

## 1. Stop Existing Servers

Kill any leftover TurboJET processes:
```bash
./scripts/stop_server.sh || true
```

Ensure all tj-api processes are dead:
```bash
pkill -9 -f tj-api 2>/dev/null || true
sleep 2
```

Verify port 8100 is free:
```bash
curl -sf --max-time 2 http://localhost:8100/api/health > /dev/null 2>&1 && echo "PORT OCCUPIED" || echo "PORT FREE"
```

If the port is still occupied, report `FATAL: Port 8100 still in use` and stop.

## 2. Build

```bash
cargo build
```

Must complete with zero errors and zero warnings. If build fails, report `FATAL: Build failed` and stop.

## 2.5 Clean Zulip Messages

Delete all bot messages from DMs and streams to start with a clean slate:

```bash
./scripts/zulip_delete_all.sh
```

This removes all TurboJet-bot and integration-bot messages from DMs and configured streams. Must succeed before proceeding.

## 4. Start Server

Start the TurboJET server:
```bash
./scripts/start_server.sh
```

This runs in the foreground. Start it in the background so you can proceed:
```bash
./scripts/start_server.sh &
SERVER_PID=$!
```

Wait for the server to become healthy (max 30 seconds):
```bash
elapsed=0
while [ "$elapsed" -lt 30 ]; do
  if curl -sf --max-time 2 http://localhost:8100/api/health > /dev/null 2>&1; then
    echo "SERVER UP"
    break
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done
```

If the server does not become healthy within 30 seconds, report `FATAL: Server failed to start` and stop.

Verify with a final health check:
```bash
curl -sf http://localhost:8100/api/health
```

Report the full JSON response. If this fails, the server is not ready.

## 5. Record Baseline

```bash
curl -sf http://localhost:8100/v1/pipeline/runs | jq 'length' || echo "0"
```

Record existing run count. Tests must account for pre-existing data.

## 6. Report Readiness

```
SETUP COMPLETE
Server: [up/down] PID: [pid]
API keys: [present/missing]
Zulip clean: [yes/no]
Existing runs: [count]
Ready for: [scenario name]
```

## Constraints

- QA manages the full server lifecycle: start in setup, stop in teardown.
- Do NOT read server code, log files, or source files during testing — including script source files. Script interfaces are documented below.
- Do NOT access the filesystem to investigate failures.
- Do NOT read server logs (`/api/logs/*`). The server is opaque. You only see what the API returns.
- If the server fails, you report "server failed" — you do not investigate why.
- Do NOT manually kill processes or run kill/pkill commands — use the scripts only.
- NEVER delete databases, logs, or Zulip messages. All data is evidence for post-test investigation.
- Record baselines (message IDs, run counts) to account for pre-existing data instead of deleting it.

## Defaults (Unless Test Scenario Specifies Otherwise)

If a test scenario does not explicitly provide different values, always use:

| Setting | Default Value |
|---------|--------------|
| Project name | `test-project` |
| Zulip channel | `test-project` — TurboJET must communicate on this channel. If it writes to any other channel, that is a BUG. |
| Test repository | `/tmp/test-repo` |

Never invent or pick alternative names. If a default doesn't work, report it as a failure — do not try a workaround.

## Zulip API Scripts

Use the provided scripts for ALL Zulip interaction. Do NOT construct curl commands against the Zulip API yourself. Do NOT read script source files — the interfaces are documented here completely.

The scripts read the endpoint and bot identities from the environment; no credentials are stored in the scripts. Export them from your ignored local environment before running setup:

```bash
: "${ZULIP_URL:?set ZULIP_URL}"
: "${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
: "${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
: "${TURBOJET_BOT_EMAIL:?set TURBOJET_BOT_EMAIL}"
```

### Send a DM to Concierge

```bash
./scripts/zulip_send_dm.sh "message text"
```

Returns the message ID on success. Capture this ID for use as `--after` in monitoring.

### Monitor DM responses

```bash
./scripts/zulip_monitor_dm.sh --after <message_id> [--timeout <seconds>]
```

- `--after <message_id>` — Required. Only return messages newer than this ID. Use the ID returned by `zulip_send_dm.sh`.
- `--timeout <seconds>` — Optional. Max wait time. Default: 30.

Example:
```bash
# Send a DM, capture the ID
MSG_ID=$(./scripts/zulip_send_dm.sh "Hello!")
# Monitor for response after that ID
timeout 30 ./scripts/zulip_monitor_dm.sh --after "$MSG_ID" --timeout 30
```

If no response arrives within the timeout, that is a test FAILURE.

### Send a stream message

```bash
./scripts/zulip_send.sh "message text" "topic" "stream"
```

- `message text` — The message content. Quote it.
- `topic` — Zulip topic within the stream.
- `stream` — Zulip stream name. Default: `test-project`.

Returns the message ID on success. Capture this ID for use as `--after` in monitoring.

### Monitor a stream

```bash
./scripts/zulip_monitor.sh --stream <stream> [--topic <topic>] --after <message_id> [--timeout <seconds>]
```

- `--stream <stream>` — Required. Stream name. Default for tests: `test-project`.
- `--topic <topic>` — Optional. Filter to a specific topic.
- `--after <message_id>` — Required. Only return messages newer than this ID.
- `--timeout <seconds>` — Optional. Max wait time. Default: 30.

Example:
```bash
# Monitor for new messages after ID 250 on test-project stream
timeout 60 ./scripts/zulip_monitor.sh --stream test-project --after 250 --timeout 60
```

### Get current messages (baseline)

To get the latest message ID for use as `--after` baseline:

```bash
./scripts/zulip_get_dm.sh       # Returns DM history with message IDs
./scripts/zulip_get_stream.sh --stream test-project  # Returns stream messages with IDs
```

## Test Modes

A test scenario can specify a mode. If no mode is specified, both API and Zulip access are allowed.

### `mode: zulip-only`

The agent MUST NOT access the TurboJET API (`http://localhost:*`) for any request during these tests. The only interaction with TurboJET is through Zulip — sending messages and reading responses.

**FORBIDDEN in zulip-only mode:**
- `curl http://localhost:*` — any request to TurboJET API
- Direct API calls for run creation, status checks, cancellations
- Any attempt to bypass the Zulip interface

**ALLOWED in zulip-only mode:**
- Sending messages via `./scripts/zulip_send.sh`
- Monitoring responses via `./scripts/zulip_monitor.sh`
- Health check (`/api/health`) — only during setup, not during test execution

**If a scenario sets `mode: zulip-only` on a test section and you need to check something (e.g., "was a run created?"), you MUST ask via Zulip chat, not via API.**

Violating this mode restriction is a test execution failure — report it honestly.
