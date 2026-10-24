#!/bin/bash
# Wait for messages on a stream (optionally filtered by topic) after a specific message ID.
# Usage: ./zulip_monitor.sh --stream <stream> [--topic <topic>] --after <message_id> [--timeout <seconds>]
# Exit 0 when responses found, 124 on timeout.

STREAM=""
TOPIC=""
AFTER_ID=""
TIMEOUT=60

while [ $# -gt 0 ]; do
  case "$1" in
    --stream)
      STREAM="$2"
      shift 2
      ;;
    --topic)
      TOPIC="$2"
      shift 2
      ;;
    --after)
      AFTER_ID="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT="$2"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      echo "Usage: $0 --stream <stream> [--topic <topic>] --after <message_id> [--timeout <seconds>]" >&2
      exit 1
      ;;
  esac
done

if [ -z "$STREAM" ]; then
  echo "Error: --stream is required" >&2
  exit 1
fi

if [ -z "$AFTER_ID" ]; then
  echo "Error: --after is required" >&2
  exit 1
fi

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"
POLL_INTERVAL=5

# Build narrow parameter: stream filter (+ optional topic filter)
if [ -n "$TOPIC" ]; then
  # URL-encoded: [{"operator":"stream","operand":"<stream>"},{"operator":"topic","operand":"<topic>"}]
  NARROW="%5B%7B%22operator%22%3A%22stream%22%2C%22operand%22%3A%22${STREAM}%22%7D%2C%7B%22operator%22%3A%22topic%22%2C%22operand%22%3A%22${TOPIC}%22%7D%5D"
else
  # URL-encoded: [{"operator":"stream","operand":"<stream>"}]
  NARROW="%5B%7B%22operator%22%3A%22stream%22%2C%22operand%22%3A%22${STREAM}%22%7D%5D"
fi

FILTER_DESC="${STREAM}"
[ -n "$TOPIC" ] && FILTER_DESC="${STREAM}/${TOPIC}"

echo "Monitoring ${FILTER_DESC} after message ${AFTER_ID} (timeout: ${TIMEOUT}s)" >&2

ELAPSED=0
while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=${AFTER_ID}&num_before=0&num_after=100&narrow=${NARROW}")

  # Extract messages with ID > AFTER_ID
  NEW_MESSAGES=$(echo "$RESPONSE" | jq -r --arg after "$AFTER_ID" \
    '.messages[] | select(.id > ($after | tonumber)) | "\(.id) | \(.subject // "general") | \(.sender_full_name): \(.content)"' 2>/dev/null)

  if [ -n "$NEW_MESSAGES" ]; then
    echo "$NEW_MESSAGES"
    exit 0
  fi

  sleep "$POLL_INTERVAL"
  ELAPSED=$((ELAPSED + POLL_INTERVAL))
done

echo "Timeout after ${TIMEOUT}s monitoring ${FILTER_DESC}" >&2
exit 124
