#!/bin/bash
# Fetch recent DM messages with TurboJet bot.
# Usage: ./zulip_get_dm.sh [--after <message_id>] [--limit <count>]
# Output: one line per message: "<id> | <sender>: <content>"
# Exit 0 always (even if no messages found).

AFTER_ID=0
LIMIT=50

while [ $# -gt 0 ]; do
  case "$1" in
    --after)
      AFTER_ID="$2"
      shift 2
      ;;
    --limit)
      LIMIT="$2"
      shift 2
      ;;
    *)
      echo "Usage: $0 [--after <message_id>] [--limit <count>]" >&2
      exit 1
      ;;
  esac
done

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
TURBOJET_BOT="${TURBOJET_BOT_EMAIL:?set TURBOJET_BOT_EMAIL}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"

NARROW="%5B%7B%22operator%22%3A%22pm-with%22%2C%22operand%22%3A%22${TURBOJET_BOT}%22%7D%5D"

if [ "$AFTER_ID" -eq 0 ]; then
  # No anchor — get the latest messages
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=1000000000&num_before=${LIMIT}&num_after=0&narrow=${NARROW}")
else
  # Get messages after the given ID
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=${AFTER_ID}&num_before=0&num_after=${LIMIT}&narrow=${NARROW}")
fi

echo "$RESPONSE" | jq -r --arg after "$AFTER_ID" \
  '.messages[] | select(.id > ($after | tonumber)) | "\(.id) | \(.sender_full_name): \(.content)"' 2>/dev/null
