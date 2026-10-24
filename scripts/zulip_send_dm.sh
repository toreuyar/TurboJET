#!/bin/bash
# Send a DM to TurboJet Concierge via Zulip API
# Usage: ./zulip_send_dm.sh "message"
# Output: ONLY the Zulip message ID on stdout (machine-parseable)
# Human-readable status on stderr

MESSAGE="$1"

if [ -z "$MESSAGE" ]; then
  echo "Usage: $0 \"message\"" >&2
  exit 1
fi

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
TURBOJET_BOT="${TURBOJET_BOT_EMAIL:?set TURBOJET_BOT_EMAIL}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"

RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
  -d "type=private" \
  -d "to=${TURBOJET_BOT}" \
  -d "content=${MESSAGE}" \
  "${ZULIP_URL}/api/v1/messages")

RESULT=$(echo "$RESPONSE" | jq -r '.result' 2>/dev/null)

if [ "$RESULT" = "success" ]; then
  MESSAGE_ID=$(echo "$RESPONSE" | jq -r '.id')
  echo "DM sent (ID: ${MESSAGE_ID})" >&2
  echo "$MESSAGE_ID"
else
  echo "Failed to send DM" >&2
  echo "$RESPONSE" | jq '.' 2>/dev/null >&2 || echo "$RESPONSE" >&2
  exit 1
fi
