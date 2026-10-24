#!/bin/bash
# Wait for DM responses from TurboJet bot after a specific message ID.
# Usage: ./zulip_monitor_dm.sh --after <message_id> [--timeout <seconds>]
# Exit 0 when responses found, 124 on timeout.

AFTER_ID=""
TIMEOUT=30

while [ $# -gt 0 ]; do
  case "$1" in
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
      echo "Usage: $0 --after <message_id> [--timeout <seconds>]" >&2
      exit 1
      ;;
  esac
done

if [ -z "$AFTER_ID" ]; then
  echo "Error: --after is required" >&2
  echo "Usage: $0 --after <message_id> [--timeout <seconds>]" >&2
  exit 1
fi

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
TURBOJET_BOT="${TURBOJET_BOT_EMAIL:?set TURBOJET_BOT_EMAIL}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"
POLL_INTERVAL=3

# URL-encoded narrow: [{"operator":"pm-with","operand":"<turbojet-bot-email>"}]
NARROW="%5B%7B%22operator%22%3A%22pm-with%22%2C%22operand%22%3A%22${TURBOJET_BOT}%22%7D%5D"

echo "Monitoring DMs after message ${AFTER_ID} (timeout: ${TIMEOUT}s)" >&2

ELAPSED=0
while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=${AFTER_ID}&num_before=0&num_after=100&narrow=${NARROW}")

  # Extract messages with ID > AFTER_ID
  NEW_MESSAGES=$(echo "$RESPONSE" | jq -r --arg after "$AFTER_ID" \
    '.messages[] | select(.id > ($after | tonumber)) | "\(.id) | \(.sender_full_name): \(.content)"' 2>/dev/null)

  if [ -n "$NEW_MESSAGES" ]; then
    echo "$NEW_MESSAGES"
    exit 0
  fi

  sleep "$POLL_INTERVAL"
  ELAPSED=$((ELAPSED + POLL_INTERVAL))
done

echo "Timeout after ${TIMEOUT}s waiting for DM responses" >&2
exit 124
