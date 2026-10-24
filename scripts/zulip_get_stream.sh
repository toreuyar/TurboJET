#!/bin/bash
# Fetch recent messages from a Zulip stream (optionally filtered by topic).
# Usage: ./zulip_get_stream.sh --stream <stream> [--topic <topic>] [--after <message_id>] [--limit <count>]
# Output: one line per message: "<id> | <topic> | <sender>: <content>"
# Exit 0 always (even if no messages found).

STREAM=""
TOPIC=""
AFTER_ID=0
LIMIT=50

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
    --limit)
      LIMIT="$2"
      shift 2
      ;;
    *)
      echo "Usage: $0 --stream <stream> [--topic <topic>] [--after <message_id>] [--limit <count>]" >&2
      exit 1
      ;;
  esac
done

if [ -z "$STREAM" ]; then
  echo "Error: --stream is required" >&2
  exit 1
fi

BOT_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
BOT_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"

if [ -n "$TOPIC" ]; then
  NARROW="%5B%7B%22operator%22%3A%22stream%22%2C%22operand%22%3A%22${STREAM}%22%7D%2C%7B%22operator%22%3A%22topic%22%2C%22operand%22%3A%22${TOPIC}%22%7D%5D"
else
  NARROW="%5B%7B%22operator%22%3A%22stream%22%2C%22operand%22%3A%22${STREAM}%22%7D%5D"
fi

if [ "$AFTER_ID" -eq 0 ]; then
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=1000000000&num_before=${LIMIT}&num_after=0&narrow=${NARROW}")
else
  RESPONSE=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=${AFTER_ID}&num_before=0&num_after=${LIMIT}&narrow=${NARROW}")
fi

echo "$RESPONSE" | jq -r --arg after "$AFTER_ID" \
  '.messages[] | select(.id > ($after | tonumber)) | "\(.id) | \(.subject // "general") | \(.sender_full_name): \(.content)"' 2>/dev/null
