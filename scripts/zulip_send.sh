#!/bin/bash
# Send a message to a Zulip stream topic.
# Usage: ./zulip_send.sh "message" "topic" "stream"
# Output: ONLY the Zulip message ID on stdout (machine-parseable)
# Human-readable status on stderr

MESSAGE="$1"
TOPIC="$2"
STREAM="$3"

if [ -z "$MESSAGE" ] || [ -z "$TOPIC" ] || [ -z "$STREAM" ]; then
  echo "Usage: $0 \"message\" \"topic\" \"stream\"" >&2
  exit 1
fi

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"

RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
  -d "type=stream" \
  -d "to=${STREAM}" \
  -d "topic=${TOPIC}" \
  -d "content=${MESSAGE}" \
  "${ZULIP_URL}/api/v1/messages")

RESULT=$(echo "$RESPONSE" | jq -r '.result' 2>/dev/null)

if [ "$RESULT" = "success" ]; then
  MESSAGE_ID=$(echo "$RESPONSE" | jq -r '.id')
  echo "Message sent to ${STREAM}/${TOPIC} (ID: ${MESSAGE_ID})" >&2
  echo "$MESSAGE_ID"
else
  echo "Failed to send message to ${STREAM}/${TOPIC}" >&2
  echo "$RESPONSE" | jq '.' 2>/dev/null >&2 || echo "$RESPONSE" >&2
  exit 1
fi
