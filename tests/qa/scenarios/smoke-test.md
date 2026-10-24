# Smoke Test

## Summary

Minimal end-to-end verification that TurboJET server starts, API endpoints respond correctly, and Concierge handles DM conversations. Tests 1-6 cover API health, models, run CRUD. Tests 7-9 cover Concierge DM responses (zulip-only mode).

**Scope:** API + Concierge DMs
**Prerequisites:** None
**Mode:** Mixed (API + zulip-only sections)

---

## Test 1: Health Check

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" http://localhost:8100/api/health
```

**Expect:** HTTP 200. Response body contains a status indicator (not an error page).

---

## Test 2: Models Listing

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" http://localhost:8100/v1/models
```

**Expect:** HTTP 200. Returns a JSON list of available models. At least one model must be present.

---

## Test 3: Create Pipeline Run

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" -X POST http://localhost:8100/v1/pipeline/runs \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Hello, this is a smoke test. Please acknowledge.", "description": "Smoke test run", "repository_url": "/tmp/test-repo"}'
```

**Expect:** HTTP 200 or 201. Returns a run object with an `id` field. Status is not `failed`.

---

## Test 4: Get Run Status

Using the `id` from Test 3:

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" http://localhost:8100/v1/pipeline/runs/{id}
```

**Expect:** HTTP 200. Returns the same run with updated state. Status field exists and is a valid value.

---

## Test 5: List Runs

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" http://localhost:8100/v1/pipeline/runs
```

**Expect:** HTTP 200. Returns an array containing at least the run created in Test 3.

---

## Test 6: Cancel Run

Using the `id` from Test 3:

**Request:**
```bash
curl -s -w "\nHTTP_STATUS:%{http_code}\n" -X POST http://localhost:8100/v1/pipeline/runs/{id}/cancel
```

**Expect:** HTTP 200. Run status changes to `cancelled` or similar terminal state.

---

## Test 7: Concierge — Initial Greeting

`mode: zulip-only` — Do NOT access TurboJET API. Interact only through Zulip.

Send a greeting to the Concierge via Zulip DM using the Integration bot.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Hello!"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```
Capture the first response from TurboJET.

**Expect:**
- TurboJET responds in the DM conversation.
- Response is a human-readable welcome message.
- Response does NOT contain raw JSON, stack traces, or technical identifiers.
- Response introduces what TurboJET can do or asks how to help.

---

## Test 8: Concierge — What Can You Do?

`mode: zulip-only` — Do NOT access TurboJET API. Interact only through Zulip.

Send a question about capabilities.

**Send:**
```bash
./scripts/zulip_send_dm.sh "What can I do with TurboJet? What tools are available to me?"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Response is in the DM conversation.
- Response is human-readable, plain language. Lists capabilities in a clear format.
- Response does NOT contain raw JSON, tool names as code, or API endpoint references.
- Response uses TurboJET terminology correctly (Projects, Lanes, Jobs, etc.).

---

## Test 9: Concierge — List Projects

`mode: zulip-only` — Do NOT access TurboJET API. Interact only through Zulip.

Ask about existing projects.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Which projects do I have available?"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Response is human-readable.
- If no projects exist, says so clearly in plain language (not "[]" or "null").
- Response is contextually relevant — answers the question that was asked, not a generic reply.
- Response does NOT expose internal IDs, timestamps, or raw data structures unless user-visible.

---

## Cleanup

Cancel any runs still in active state. Teardown handles server stop.
